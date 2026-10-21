//! The interface running: keys and backend events in, drawing and backend calls out.

use std::collections::HashSet;
use std::io::{self, Write, stdout};
use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEventKind};
use crossterm::terminal;
use futures::{FutureExt, Stream, StreamExt};

use crate::backend::{Backend, Downloaded, Draft, Event};
use crate::data::{self, ChatId, Discussion, Folder, MessageId};
use crate::graphics::Thumbnails;
use crate::settings::Settings;
use crate::store::{Pins, ReadMarks};
use crate::tui::{Action, App, TerminalGuard};

/// Where an application keeps its state.
#[derive(Debug, Clone)]
pub struct Config {
    /// Name of the application, shown in its desktop notifications.
    pub name: String,
    /// Version of the application, after its name above the chat list; none if empty.
    pub version: String,
    pub settings: PathBuf,
    /// When each chat was last read.
    pub read_marks: PathBuf,
    pub pins: PathBuf,
    /// Chat opened at start, instead of the chat list.
    pub open: Option<ChatId>,
}

impl Config {
    /// The usual places for application `name`: its settings in `~/.config/<name>/`, its
    /// state in `~/.local/share/<name>/` (or the XDG directories).
    pub fn new(name: &str) -> io::Result<Self> {
        let state = xdg_dir("XDG_DATA_HOME", ".local/share")?.join(name);
        Ok(Config {
            name: name.to_string(),
            version: String::new(),
            settings: xdg_dir("XDG_CONFIG_HOME", ".config")?
                .join(name)
                .join("settings"),
            read_marks: state.join("read"),
            pins: state.join("pinned"),
            open: None,
        })
    }
}

/// `$var` when it is an absolute path, else `~/<fallback>`.
fn xdg_dir(var: &str, fallback: &str) -> io::Result<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback)))
        .ok_or_else(|| io::Error::other(format!("neither {var} nor HOME is set")))
}

/// Runs the interface on `discussions` until the user quits: what they do goes to
/// `backend`, and `events` (received messages…) are shown as they come.
///
/// Must run in a [`tokio::task::LocalSet`]: background downloads are local tasks.
pub async fn run(
    backend: impl Backend,
    mut discussions: Vec<Discussion>,
    mut events: impl Stream<Item = Event> + Unpin,
    config: Config,
) -> io::Result<()> {
    let mut read_marks = ReadMarks::load(config.read_marks.clone());
    let mut pins = Pins::load(config.pins.clone());
    for discussion in &mut discussions {
        discussion.unread = read_marks.unread_count(discussion);
        discussion.pinned = pins.rank(&discussion.id);
    }
    let mut app = App::new(discussions);
    app.name = config.name.clone();
    app.version = config.version.clone();
    app.settings = Settings::load(&config.settings);
    app.thumbnails = Thumbnails::detect();
    app.capabilities = backend.capabilities();

    let mut events_open = true;
    // Images of the history, downloaded in the background for their thumbnails.
    let (fetched_tx, mut fetched) = futures::channel::mpsc::unbounded();
    let mut history_fetched = HashSet::new();

    if let Some(idx) = config
        .open
        .as_ref()
        .and_then(|chat| app.discussions.iter().position(|d| d.id == *chat))
    {
        app.open(idx);
        opened(
            &mut app,
            idx,
            &backend,
            &mut read_marks,
            &mut history_fetched,
            &fetched_tx,
        );
    }

    let _terminal = TerminalGuard::enter()?;
    let mut keys = EventStream::new();
    let mut out = stdout();
    loop {
        let (w, h) = terminal::size()?;
        app.draw(&mut out, w, h)?;
        out.flush()?;

        let typing_expiry = app.next_typing_expiry();
        tokio::select! {
            _ = tokio::time::sleep_until(typing_expiry.unwrap_or_else(Instant::now).into()), if typing_expiry.is_some() => {
                app.expire_typing(Instant::now());
            }
            key = keys.next() => {
                let Some(key) = key else { break };
                let action = match key? {
                    TermEvent::Key(key) => app.handle_key(key),
                    TermEvent::Paste(text) => {
                        app.handle_paste(&text);
                        None
                    }
                    _ => None,
                };
                let Some(action) = action else { continue };

                let (discussion, draft) = match action {
                    Action::Quit => break,
                    Action::React { discussion, msg, emoji, remove } => {
                        let chat = &app.discussions[discussion];
                        match backend.react(&chat.id, &chat.messages[msg], &emoji, remove).await {
                            Ok(()) => {
                                let emoji = (!remove).then_some(emoji.as_str());
                                app.discussions[discussion].messages[msg].set_reaction("", true, emoji);
                            }
                            Err(e) => app.set_status(format!("Reaction failed: {e}")),
                        }
                        continue;
                    }
                    Action::Archive(chats) => {
                        file_chats(&mut app, &backend, &mut out, (w, h), ChatOp::Archive, chats).await?;
                        continue;
                    }
                    Action::DeleteChat(chats) => {
                        file_chats(&mut app, &backend, &mut out, (w, h), ChatOp::Delete, chats).await?;
                        continue;
                    }
                    Action::PurgeTrash => {
                        let trash = app.trash();
                        file_chats(&mut app, &backend, &mut out, (w, h), ChatOp::Purge, trash).await?;
                        continue;
                    }
                    Action::NewChat => {
                        app.set_status("Loading the contacts\u{2026}");
                        app.draw(&mut out, w, h)?;
                        out.flush()?;
                        let contacts = backend.contacts().await;
                        app.clear_status();
                        match contacts {
                            Ok(contacts) => app.new_chat(contacts),
                            Err(e) => {
                                app.new_chat(Vec::new());
                                app.set_status(format!("Cannot load the contacts: {e}"));
                            }
                        }
                        continue;
                    }
                    Action::StartChat(numbers) => {
                        app.set_status("Starting the chat\u{2026}");
                        app.draw(&mut out, w, h)?;
                        out.flush()?;
                        let started = backend.start_chat(numbers).await;
                        app.clear_status();
                        match started {
                            Ok(mut discussion) => {
                                let idx = match app.discussions.iter().position(|d| d.id == discussion.id) {
                                    Some(idx) => {
                                        app.update_discussion(idx, discussion);
                                        idx
                                    }
                                    None => {
                                        discussion.pinned = pins.rank(&discussion.id);
                                        discussion.unread = read_marks.unread_count(&discussion);
                                        app.push_discussion(discussion)
                                    }
                                };
                                if let Action::Opened(idx) = app.started(idx) {
                                    opened(&mut app, idx, &backend, &mut read_marks, &mut history_fetched, &fetched_tx);
                                }
                            }
                            Err(e) => app.set_status(format!("Cannot start the chat: {e}")),
                        }
                        continue;
                    }
                    Action::Restore(chats) => {
                        file_chats(&mut app, &backend, &mut out, (w, h), ChatOp::Restore, chats).await?;
                        continue;
                    }
                    Action::Delete { discussion, msg } => {
                        let chat = &app.discussions[discussion];
                        match backend.delete(&chat.id, &chat.messages[msg]).await {
                            Ok(()) => app.discussions[discussion].messages[msg].mark_deleted(),
                            Err(e) => app.set_status(format!("Deletion failed: {e}")),
                        }
                        continue;
                    }
                    Action::PasteImageFromClipboard => {
                        if let Some(attachment) = data::Attachment::from_clipboard() {
                            app.handle_pasted_image(attachment);
                        } else {
                            app.set_status("No image in the clipboard");
                        }
                        continue;
                    }
                    Action::FetchAttachments { discussion, msg } => {
                        app.set_status("Downloading\u{2026}");
                        app.draw(&mut out, w, h)?;
                        out.flush()?;
                        let chat = &app.discussions[discussion];
                        match backend.download(&chat.id, &chat.messages[msg]).await {
                            Ok((attachments, images)) => {
                                app.discussions[discussion].messages[msg].set_downloaded(attachments, images);
                                app.open_selection();
                            }
                            Err(e) => {
                                app.discussions[discussion].messages[msg].download_failed = true;
                                app.set_status(format!("Download failed: {e}"));
                            }
                        }
                        continue;
                    }
                    Action::PinToggled(chats) => {
                        for idx in chats {
                            let chat = &app.discussions[idx];
                            if let Err(e) = pins.set(&chat.id, chat.pinned.is_some()) {
                                app.set_status(format!("Cannot save the pinned chats: {e}"));
                            }
                        }
                        continue;
                    }
                    Action::PinsReordered => {
                        let mut pinned: Vec<&Discussion> =
                            app.discussions.iter().filter(|d| d.pinned.is_some()).collect();
                        pinned.sort_by_key(|d| d.pinned);
                        if let Err(e) = pins.set_order(pinned.iter().map(|d| d.id.clone()).collect()) {
                            app.set_status(format!("Cannot save the pinned chats: {e}"));
                        }
                        continue;
                    }
                    Action::SettingsChanged => {
                        if let Err(e) = app.settings.save(&config.settings) {
                            app.set_status(format!("Cannot save the parameters: {e}"));
                        }
                        continue;
                    }
                    Action::Opened(idx) => {
                        opened(&mut app, idx, &backend, &mut read_marks, &mut history_fetched, &fetched_tx);
                        continue;
                    }
                    Action::Send { discussion, text, styles, reply_to } => {
                        (discussion, draft(&app.discussions[discussion], text, styles, Vec::new(), reply_to))
                    }
                    Action::SendWithAttachments { discussion, text, attachments, reply_to } => {
                        (discussion, draft(&app.discussions[discussion], text, Vec::new(), attachments, reply_to))
                    }
                    Action::Forward { from, msg, to } => {
                        let message = &app.discussions[from].messages[msg];
                        let mut draft = draft(&app.discussions[to], message.text.clone(), message.styles.clone(), Vec::new(), None);
                        draft.forwarded = true;
                        (to, draft)
                    }
                };
                app.set_status("Sending\u{2026} (Esc to cancel)");
                app.draw(&mut out, w, h)?;
                out.flush()?;

                let forwarded = draft.forwarded;
                let reply_to = draft.reply_to.as_ref().and_then(|m| app.discussions[discussion].position_of(&m.id));
                let mut send = std::pin::pin!(backend.send(&app.discussions[discussion].id, draft));
                // Waiting for it, whatever blocks it: Esc gives up (other keys wait).
                let sent = loop {
                    tokio::select! {
                        sent = &mut send => break Some(sent),
                        key = keys.next() => match key {
                            Some(Ok(TermEvent::Key(key)))
                                if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc =>
                            {
                                break None;
                            }
                            None => break None,
                            _ => {}
                        },
                    }
                };
                let Some(sent) = sent else {
                    app.set_status("Sending cancelled: it may still go out if it had left");
                    continue;
                };
                match sent {
                    Ok(mut message) => {
                        message.forwarded = forwarded;
                        message.reply_to = reply_to;
                        app.push_message(discussion, message);
                        if forwarded {
                            app.set_status(format!("Forwarded to {}", app.discussions[discussion].title));
                        } else {
                            app.set_status("Sent");
                        }
                    }
                    Err(e) => app.set_status(format!("Sending failed: {e}")),
                }
            }
            Some((chat, id, downloaded)) = fetched.next() => {
                if let Some(message) = app
                    .discussions
                    .iter_mut()
                    .find(|d| d.id == chat)
                    .and_then(|d| d.messages.iter_mut().find(|m| m.id == id && m.needs_download()))
                {
                    match downloaded {
                        Some((attachments, images)) => message.set_downloaded(attachments, images),
                        None => message.download_failed = true,
                    }
                }
            }
            event = events.next(), if events_open => {
                let Some(event) = event else {
                    events_open = false;
                    app.set_status("Disconnected: restart to receive new messages");
                    continue;
                };
                // History come after the chat was opened: its images too. A new message: its
                // images on arrival.
                let images_of = match &event {
                    Event::History { chat, .. } if history_fetched.contains(chat) => Some((chat.clone(), None)),
                    Event::Message { chat, message, .. } if app.thumbnails.enabled() => {
                        Some((chat.clone(), Some(message.id.clone())))
                    }
                    _ => None,
                };
                handle_event(&mut app, event, &backend, &mut read_marks, &config);
                if let Some((chat, only)) = images_of
                    && let Some(idx) = app.discussions.iter().position(|d| d.id == chat)
                {
                    fetch_images(&backend, &app.discussions[idx], only.as_ref(), &fetched_tx);
                }
            }
        }
    }
    let _ = app.thumbnails.clear(&mut out);
    Ok(())
}

/// Discussion `idx` was opened: it is read, and its history images start downloading
/// (once per run).
fn opened(
    app: &mut App,
    idx: usize,
    backend: &impl Backend,
    read_marks: &mut ReadMarks,
    history_fetched: &mut HashSet<ChatId>,
    fetched: &Fetched,
) {
    read(app, idx, backend, read_marks);
    if app.thumbnails.enabled() && history_fetched.insert(app.discussions[idx].id.clone()) {
        fetch_images(backend, &app.discussions[idx], None, fetched);
    }
}

/// The draft of a message to `discussion`, replying to its message `reply_to`.
fn draft(
    discussion: &Discussion,
    text: String,
    styles: Vec<data::Styled>,
    attachments: Vec<data::Attachment>,
    reply_to: Option<usize>,
) -> Draft {
    Draft {
        text,
        styles,
        attachments,
        reply_to: reply_to.and_then(|i| discussion.messages.get(i).cloned()),
        forwarded: false,
    }
}

/// Shows what the backend reports.
fn handle_event(
    app: &mut App,
    event: Event,
    backend: &impl Backend,
    read_marks: &mut ReadMarks,
    config: &Config,
) {
    let position = |app: &App, chat: &str| app.discussions.iter().position(|d| d.id == chat);
    match event {
        Event::Chat(discussion) => match position(app, &discussion.id) {
            Some(idx) => app.update_discussion(idx, discussion),
            None => {
                app.push_discussion(discussion);
            }
        },
        Event::Message {
            chat,
            mut message,
            quote,
        } => {
            let Some(idx) = position(app, &chat) else {
                return;
            };
            message.reply_to = quote.and_then(|id| app.discussions[idx].position_of(&id));
            // Known already (sent from here, updated): replaced, not notified.
            let new = app.discussions[idx].position_of(&message.id).is_none();
            if new && app.settings.notifications && !message.from_me && !app.is_open(idx) {
                let discussion = &app.discussions[idx];
                let body = notice_body(discussion, &message.sender_name, &message.summary());
                notify(&config.name, &discussion.title, &body);
            }
            // Written to: listed again (as the phones do).
            if new && !message.from_me {
                app.discussions[idx].hidden = false;
            }
            app.push_message(idx, message);
            // Arrived in the discussion being read: not new on the next start.
            if app.is_open(idx) {
                read(app, idx, backend, read_marks);
            }
        }
        Event::History { chat, messages } => {
            let Some(idx) = position(app, &chat) else {
                return;
            };
            app.merge_history(idx, messages);
            if app.is_open(idx) {
                read(app, idx, backend, read_marks);
            } else {
                let discussion = &mut app.discussions[idx];
                discussion.unread = read_marks.unread_count(discussion);
            }
        }
        Event::Reaction {
            chat,
            target,
            author,
            mine,
            emoji,
        } => {
            if let Some(idx) = position(app, &chat)
                && let Some(i) = app.discussions[idx].position_of(&target)
            {
                app.discussions[idx].messages[i].set_reaction(&author, mine, emoji.as_deref());
            }
        }
        Event::Deleted {
            chat,
            target,
            author,
        } => {
            if let Some(idx) = position(app, &chat)
                && let Some(message) = app.discussions[idx]
                    .messages
                    .iter_mut()
                    .find(|m| m.id == target && m.author.as_ref() == Some(&author))
            {
                message.mark_deleted();
            }
        }
        Event::Typing {
            chat,
            author,
            name,
            started,
        } => {
            if let Some(idx) = position(app, &chat) {
                app.set_typing(idx, &author, &name, started, Instant::now());
            }
        }
        Event::Read { chat } => {
            if let Some(idx) = position(app, &chat) {
                app.discussions[idx].unread = 0;
                if let Err(e) = read_marks.mark_read(&app.discussions[idx]) {
                    app.set_status(format!("Cannot save the read state: {e}"));
                }
            }
        }
        Event::Missed(messages) => {
            if app.settings.notifications
                && let Some((summary, body)) = missed_notice(app, &messages)
            {
                notify(&config.name, &summary, &body);
            }
        }
        Event::Status(status) => app.set_status(status),
    }
}

/// The notification of `missed` messages, those still unread in chats not open (spam and
/// trash left out): as for a message received if there is one, else their number and their
/// chats.
fn missed_notice(app: &App, missed: &[(ChatId, MessageId)]) -> Option<(String, String)> {
    // By chat, in the order of `missed`: the chat and its messages.
    let mut chats: Vec<(usize, Vec<usize>)> = Vec::new();
    for (chat, id) in missed {
        let Some(idx) = app.discussions.iter().position(|d| &d.id == chat) else {
            continue;
        };
        let discussion = &app.discussions[idx];
        if discussion.hidden
            || app.is_open(idx)
            || matches!(discussion.folder, Some(Folder::Spam | Folder::Trash))
        {
            continue;
        }
        let Some(i) = discussion.position_of(id) else {
            continue;
        };
        if discussion.messages[i].from_me || discussion.first_unread().is_none_or(|first| i < first)
        {
            continue;
        }
        match chats.iter_mut().find(|(c, _)| *c == idx) {
            Some((_, messages)) if !messages.contains(&i) => messages.push(i),
            Some(_) => {}
            None => chats.push((idx, vec![i])),
        }
    }
    match chats.as_slice() {
        [] => None,
        [(idx, messages)] if messages.len() == 1 => {
            let discussion = &app.discussions[*idx];
            let message = &discussion.messages[messages[0]];
            let body = notice_body(discussion, &message.sender_name, &message.summary());
            Some((discussion.title.clone(), body))
        }
        _ => {
            let count: usize = chats.iter().map(|(_, messages)| messages.len()).sum();
            let body = chats
                .iter()
                .map(|(idx, messages)| match messages.len() {
                    1 => app.discussions[*idx].title.clone(),
                    n => format!("{} ({n})", app.discussions[*idx].title),
                })
                .collect::<Vec<_>>()
                .join(", ");
            Some((format!("{count} new messages"), body))
        }
    }
}

/// Discussion `idx` was read up to its last message: here, then on the other devices.
fn read(app: &mut App, idx: usize, backend: &impl Backend, read_marks: &mut ReadMarks) {
    let discussion = &app.discussions[idx];
    if let Some(last) = discussion.messages.last() {
        backend.read(&discussion.id, last);
    }
    if let Err(e) = read_marks.mark_read(discussion) {
        app.set_status(format!("Cannot save the read state: {e}"));
    }
}

/// Downloads in the background, newest first, the history images of `discussion` (with
/// the images of the link previews), or only those of its message `only`, sent to `fetched`
/// as they come. Messages with other attachments (videos, files…) are left for `o`.
fn fetch_images(
    backend: &impl Backend,
    discussion: &Discussion,
    only: Option<&MessageId>,
    fetched: &Fetched,
) {
    let downloads: Vec<_> = discussion
        .messages
        .iter()
        .rev()
        .filter(|m| only.is_none_or(|id| &m.id == id))
        .filter(|m| m.needs_download() && m.images_only())
        .map(|m| (m.id.clone(), backend.download(&discussion.id, m)))
        .collect();
    if downloads.is_empty() {
        return;
    }
    let (chat, fetched) = (discussion.id.clone(), fetched.clone());
    tokio::task::spawn_local(async move {
        for (id, download) in downloads {
            let downloaded = download.await.ok();
            if fetched
                .unbounded_send((chat.clone(), id, downloaded))
                .is_err()
            {
                break;
            }
        }
    });
}

/// Where the downloaded history images go: (chat, message, what was downloaded, `None`
/// when it failed).
type Fetched = futures::channel::mpsc::UnboundedSender<(String, MessageId, Option<Downloaded>)>;

/// The notification body of a message received in `discussion`: its text, prefixed by the
/// sender in groups.
fn notice_body(discussion: &Discussion, sender_name: &str, text: &str) -> String {
    if discussion.is_group {
        format!("{sender_name}: {text}")
    } else {
        text.to_string()
    }
}

/// Desktop notification: for a received message, the chat as `summary` and the text as
/// `body`.
fn notify(app_name: &str, summary: &str, body: &str) {
    let (app_name, summary, body) = (app_name.to_string(), summary.to_string(), body.to_string());
    // Notification servers block on D-Bus / OS calls: keep them off the event loop.
    // Failures (no notification server…) are not worth interrupting the user for.
    tokio::task::spawn_blocking(move || {
        let _ = notify_rust::Notification::new()
            .appname(&app_name)
            .summary(&summary)
            .body(&escape_markup(&body))
            .show();
    });
}

/// The freedesktop notification body is markup: `<`, `>` and `&` must be escaped there.
fn escape_markup(text: &str) -> String {
    if cfg!(all(unix, not(target_os = "macos"))) {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    } else {
        text.to_string()
    }
}

/// Chats acted on at once ([`file_chats`]).
const CHATS_AT_ONCE: usize = 8;

/// What is done to chats of the list, one by one.
#[derive(Clone, Copy, PartialEq)]
enum ChatOp {
    Archive,
    Restore,
    Delete,
    /// Delete, the whole trash.
    Purge,
}

/// Does `op` on `chats`, a few at once, showing the progress when several; then says how it
/// went.
async fn file_chats(
    app: &mut App,
    backend: &impl Backend,
    out: &mut impl Write,
    (w, h): (u16, u16),
    op: ChatOp,
    chats: Vec<usize>,
) -> io::Result<()> {
    let (doing, done, failing) = match op {
        ChatOp::Archive => ("Archiving", "Archived", "Archiving failed"),
        ChatOp::Restore => ("Restoring", "Restored", "Restoring failed"),
        ChatOp::Delete => ("Deleting", "Deleted", "Deletion failed"),
        ChatOp::Purge => ("Purging the trash", "Purged", "Purging failed"),
    };
    let mut ok = 0;
    let mut failed = None;
    let ids: Vec<(usize, ChatId)> = chats
        .iter()
        .map(|&chat| (chat, app.discussions[chat].id.clone()))
        .collect();
    // A few at once: each waits for the phone.
    let mut results = futures::stream::iter(ids)
        .map(|(chat, id)| {
            let result = match op {
                ChatOp::Archive => backend.archive(&id).boxed_local(),
                ChatOp::Restore => backend.restore(&id).boxed_local(),
                ChatOp::Delete | ChatOp::Purge => backend.delete_chat(&id).boxed_local(),
            };
            async move { (chat, result.await) }
        })
        .buffer_unordered(CHATS_AT_ONCE);
    for n in 0.. {
        if chats.len() > 1 {
            app.set_status(format!("{doing}\u{2026} {n}/{}", chats.len()));
            app.draw(out, w, h)?;
            out.flush()?;
        }
        let Some((chat, result)) = results.next().await else {
            break;
        };
        match result {
            Ok(()) => {
                ok += 1;
                match op {
                    ChatOp::Archive => app.file_discussion(chat, Some(data::Folder::Archive)),
                    ChatOp::Restore => app.file_discussion(chat, None),
                    // Its new place (trash, or gone) comes from the backend.
                    ChatOp::Delete | ChatOp::Purge => app.hide_discussion(chat),
                }
            }
            Err(e) => failed = Some(e),
        }
    }
    let status = match (&chats[..], failed) {
        ([chat], None) if op == ChatOp::Restore => format!(
            "\u{201c}{}\u{201d} is back in the inbox",
            app.discussions[*chat].title
        ),
        ([chat], None) => format!("{done} \u{201c}{}\u{201d}", app.discussions[*chat].title),
        ([_], Some(e)) => format!("{failing}: {e}"),
        (_, None) if op == ChatOp::Restore => format!("{ok} chats are back in the inbox"),
        (_, None) => format!("{done} {ok} chats"),
        (_, Some(e)) => format!("{done} {ok} chats, {} failed: {e}", chats.len() - ok),
    };
    app.set_status(status);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Message;

    fn received(id: &str, sender: &str) -> Message {
        let mut m = Message::mine(id, format!("text {id}"), None, false);
        m.from_me = false;
        m.sender_name = sender.to_string();
        m
    }

    fn chat(id: &str, messages: &[&str], unread: usize) -> Discussion {
        let mut d = Discussion::new(id, id.to_uppercase());
        d.messages = messages.iter().map(|m| received(m, "Ann")).collect();
        d.unread = unread;
        d
    }

    fn missed(ids: &[(&str, &str)]) -> Vec<(ChatId, MessageId)> {
        ids.iter()
            .map(|(c, m)| (c.to_string(), m.to_string()))
            .collect()
    }

    #[test]
    fn one_missed_message_is_notified_as_received() {
        let app = App::new(vec![chat("a", &["a1", "a2"], 1)]);
        assert_eq!(
            missed_notice(&app, &missed(&[("a", "a2")])),
            Some(("A".to_string(), "text a2".to_string()))
        );
        assert_eq!(
            missed_notice(&app, &missed(&[("a", "a1")])),
            None,
            "read already"
        );
    }

    #[test]
    fn missed_messages_are_summed_up_by_chat() {
        let mut spam = chat("s", &["s1"], 1);
        spam.folder = Some(Folder::Spam);
        let app = App::new(vec![
            chat("a", &["a1", "a2"], 2),
            chat("b", &["b1"], 1),
            chat("c", &["c1"], 0),
            spam,
        ]);
        let notice = missed_notice(
            &app,
            &missed(&[
                ("b", "b1"),
                ("a", "a1"),
                ("a", "a2"),
                ("c", "c1"),
                ("s", "s1"),
                ("x", "x1"),
            ]),
        );
        assert_eq!(
            notice,
            Some(("3 new messages".to_string(), "B, A (2)".to_string()))
        );
    }
}
