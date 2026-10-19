//! The interface running: keys and backend events in, drawing and backend calls out.

use std::collections::HashSet;
use std::io::{self, Write, stdout};
use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEventKind};
use crossterm::terminal;
use futures::{Stream, StreamExt};

use crate::backend::{Backend, Downloaded, Draft, Event};
use crate::data::{self, ChatId, Discussion, MessageId};
use crate::graphics::Thumbnails;
use crate::settings::Settings;
use crate::store::{Pins, ReadMarks};
use crate::tui::{Action, App, TerminalGuard};

/// Where an application keeps its state.
#[derive(Debug, Clone)]
pub struct Config {
    /// Name of the application, shown in its desktop notifications.
    pub name: String,
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
                    Action::Archive(discussion) => {
                        let chat = &app.discussions[discussion];
                        let title = chat.title.clone();
                        match backend.archive(&chat.id).await {
                            Ok(()) => {
                                app.hide_discussion(discussion);
                                app.set_status(format!("Archived \u{201c}{title}\u{201d}"));
                            }
                            Err(e) => app.set_status(format!("Archiving failed: {e}")),
                        }
                        continue;
                    }
                    Action::DeleteChat(discussion) => {
                        let chat = &app.discussions[discussion];
                        let title = chat.title.clone();
                        match backend.delete_chat(&chat.id).await {
                            Ok(()) => {
                                app.hide_discussion(discussion);
                                app.discussions[discussion].messages.clear();
                                app.discussions[discussion].unread = 0;
                                app.set_status(format!("Deleted \u{201c}{title}\u{201d}"));
                            }
                            Err(e) => app.set_status(format!("Deletion failed: {e}")),
                        }
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
                    Action::PinToggled(idx) => {
                        let chat = &app.discussions[idx];
                        if let Err(e) = pins.set(&chat.id, chat.pinned.is_some()) {
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
                notify(
                    &config.name,
                    &app.discussions[idx],
                    &message.sender_name,
                    &message.summary(),
                );
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
        Event::Status(status) => app.set_status(status),
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

/// Desktop notification for a received message: the chat as title, the text as body
/// (prefixed by the sender in groups).
fn notify(app_name: &str, discussion: &Discussion, sender_name: &str, text: &str) {
    let summary = discussion.title.clone();
    let body = if discussion.is_group {
        format!("{sender_name}: {text}")
    } else {
        text.to_string()
    };
    let app_name = app_name.to_string();
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
