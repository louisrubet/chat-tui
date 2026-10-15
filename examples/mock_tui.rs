//! The interface with fake chats, no network: sending only appends locally.
//!
//!   cargo run --example mock_tui            # `-- --open` opens the first chat
//!
//! Its state (pins, read marks, settings) goes to a temporary directory.

use std::future::Future;
use std::io;
use std::time::Duration;

use chat_tui::backend::Result;
use chat_tui::data::{self, ChatId, Message};
use chat_tui::{Backend, Capabilities, Config, Downloaded, Draft};

/// Sends locally, and has nothing to download.
struct Mock;

// `async fn` would borrow `self` and the arguments: the trait wants `'static` futures.
#[allow(clippy::manual_async_fn)]
impl Backend for Mock {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            delete_max_age: Some(Duration::from_secs(24 * 60 * 60)),
        }
    }

    fn send(
        &self,
        _chat: &ChatId,
        draft: Draft,
    ) -> impl Future<Output = Result<Message>> + 'static {
        async move {
            let id = chrono::Utc::now().timestamp_millis();
            let mut message = Message::mine(id, draft.text, None, draft.forwarded);
            message.styles = draft.styles;
            message.attachments = draft.attachments;
            Ok(message)
        }
    }

    fn react(
        &self,
        _chat: &ChatId,
        _message: &Message,
        _emoji: &str,
        _remove: bool,
    ) -> impl Future<Output = Result<()>> + 'static {
        async { Ok(()) }
    }

    fn delete(
        &self,
        _chat: &ChatId,
        _message: &Message,
    ) -> impl Future<Output = Result<()>> + 'static {
        async { Ok(()) }
    }

    fn download(
        &self,
        _chat: &ChatId,
        _message: &Message,
    ) -> impl Future<Output = Result<Downloaded>> + 'static {
        async { Err("nothing to download in the mock".into()) }
    }
}

fn main() -> io::Result<()> {
    let mut discussions = data::mock_discussions();
    discussions.insert(0, demo::discussion());

    // The pins and unread counts above, as the state files `run` reads them from.
    let state = std::env::temp_dir().join(format!("chat-tui-mock-{}", std::process::id()));
    std::fs::create_dir_all(&state)?;
    std::fs::write(state.join("pinned"), "lisbon\nalice\n")?;
    let marks: String = discussions
        .iter()
        .map(|d| {
            let first = d.first_unread().unwrap_or(d.messages.len());
            let read = first.checked_sub(1).map_or(0, |i| d.messages[i].sent_ms);
            format!("{} {read}\n", d.id)
        })
        .collect();
    std::fs::write(state.join("read"), marks)?;
    let config = Config {
        name: "chat-tui mock".to_string(),
        settings: state.join("settings"),
        read_marks: state.join("read"),
        pins: state.join("pinned"),
        open: std::env::args()
            .any(|arg| arg == "--open")
            .then(|| "lisbon".to_string()),
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(tokio::task::LocalSet::new().run_until(chat_tui::run(
        Mock,
        discussions,
        futures::stream::pending(),
        config,
    )));
    let _ = std::fs::remove_dir_all(&state);
    result
}

/// A chat showing what the interface can do: photos, link previews, emojis, reactions
/// (the screenshot of the README).
mod demo {
    use chat_tui::data::{Attachment, Discussion, LinkPreview, Message, Style, Styled};
    use chrono::{Duration, Local, NaiveTime};
    use image::{Rgb, RgbImage};

    pub fn discussion() -> Discussion {
        let today = Local::now().date_naive();
        let at = |days: i64, h: u32, m: u32| {
            (today - Duration::days(days)).and_time(NaiveTime::from_hms_opt(h, m, 0).unwrap())
        };
        let from = |name: &str, text: &str, time| Message {
            from_me: false,
            author: Some(name.to_string()),
            sender_name: name.to_string(),
            timestamp: time,
            ..Message::mine(0, text.to_string(), None, false)
        };
        let mine = |text: &str, time| Message {
            timestamp: time,
            ..Message::mine(0, text.to_string(), None, false)
        };

        let mut sunset = from(
            "Alice Martin",
            "Landed! The light here is unreal \u{1f305}",
            at(0, 9, 42),
        );
        sunset.attachments = vec![jpeg(include_bytes!("demo/sunset.jpg"))];
        sunset.set_reaction("me", true, Some("\u{2764}\u{fe0f}"));
        sunset.set_reaction("Bob Dupont", false, Some("\u{1f60d}"));

        let mut alfama = from(
            "Bob Dupont",
            "Dinner tonight is here: https://en.wikipedia.org/wiki/Alfama",
            at(0, 17, 5),
        );
        alfama.previews = vec![LinkPreview {
            url: "https://en.wikipedia.org/wiki/Alfama".to_string(),
            title: "Alfama - Wikipedia".to_string(),
            description: "The oldest neighborhood of Lisbon, spreading on the slope between the \
                          S\u{e3}o Jorge Castle and the Tagus river."
                .to_string(),
            image: Some(jpeg(include_bytes!("demo/alfama.jpg"))),
            remote_image: false,
        }];

        let mut booked = from(
            "Alice Martin",
            "Perfect, I'll book for 8 \u{1f377}",
            at(0, 17, 9),
        );
        booked.reply_to = Some(2);
        booked.styles = vec![Styled {
            start: 0,
            end: 7,
            style: Style::Bold,
        }];

        let mut app = mine(
            "The app I told you about \u{1f447} https://github.com/louisrubet/signal-tui",
            at(0, 17, 30),
        );
        app.previews = vec![LinkPreview {
            url: "https://github.com/louisrubet/signal-tui".to_string(),
            title: "louisrubet/signal-tui".to_string(),
            description: "A Signal client for the terminal: thumbnails, link previews, \
                          reactions, replies."
                .to_string(),
            image: Some(png(card_image(480, 240))),
            remote_image: false,
        }];
        app.set_reaction("Alice Martin", false, Some("\u{1f44d}"));

        let mut tram = from("Bob Dupont", "Tram 28, obviously \u{1f68b}", at(0, 18, 2));
        tram.styles = vec![Styled {
            start: 9,
            end: 18,
            style: Style::Italic,
        }];
        tram.attachments = vec![jpeg(include_bytes!("demo/tram.jpg"))];

        let mut messages = vec![
            sunset,
            mine("Wow \u{1f60d} save me a pastel de nata!", at(0, 9, 45)),
            alfama,
            booked,
            app,
            tram,
        ];
        for message in &mut messages {
            message.sent_ms = message.timestamp.and_utc().timestamp_millis() as u64;
            message.id = message.sent_ms.to_string();
        }
        Discussion {
            id: "lisbon".to_string(),
            title: "Lisbon trip \u{1f30d}".to_string(),
            is_group: true,
            messages,
            unread: 1,
            pinned: Some(0),
        }
    }

    /// A photo of `examples/demo` (credits in its README).
    fn jpeg(bytes: &[u8]) -> Attachment {
        Attachment {
            content_type: "image/jpeg".to_string(),
            file_name: None,
            width: None,
            height: None,
            bytes: bytes.to_vec(),
        }
    }

    fn png(image: RgbImage) -> Attachment {
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("PNG encoding");
        Attachment {
            content_type: "image/png".to_string(),
            file_name: None,
            width: Some(image.width()),
            height: Some(image.height()),
            bytes,
        }
    }

    fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> Rgb<u8> {
        let t = t.clamp(0.0, 1.0);
        Rgb([0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t) as u8))
    }

    /// A social card: a terminal window on a dark background.
    fn card_image(w: u32, h: u32) -> RgbImage {
        let (left, top, right, bottom) = (w / 6, h / 5, w * 5 / 6, h * 4 / 5);
        RgbImage::from_fn(w, h, |x, y| {
            let inside = (left..right).contains(&x) && (top..bottom).contains(&y);
            if !inside {
                return mix([13, 17, 23], [40, 50, 90], (x + y) as f32 / (w + h) as f32);
            }
            if y < top + 18 {
                let dot = |i: u32| {
                    (x as i32 - (left + 16 + i * 18) as i32).pow(2)
                        + (y as i32 - (top + 9) as i32).pow(2)
                        < 30
                };
                if dot(0) {
                    return Rgb([255, 95, 86]);
                }
                if dot(1) {
                    return Rgb([255, 189, 46]);
                }
                if dot(2) {
                    return Rgb([39, 201, 63]);
                }
                return Rgb([60, 64, 72]);
            }
            let line = (y - top - 18) / 22;
            let col = x - left;
            let lengths = [140, 220, 90, 180, 120];
            if (y - top - 18) % 22 > 8
                && (y - top - 18) % 22 < 16
                && col > 16
                && col < 16 + lengths[line as usize % 5]
            {
                return if line == 0 {
                    Rgb([58, 118, 240])
                } else {
                    Rgb([170, 180, 190])
                };
            }
            Rgb([30, 33, 40])
        })
    }
}
