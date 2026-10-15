//! What a messaging service provides to the interface: a [`Backend`] carrying out what the
//! user does, and a stream of [`Event`]s for what happens elsewhere.

use std::error::Error;
use std::future::Future;
use std::time::Duration;

use crate::data::{Attachment, ChatId, Discussion, Message, MessageId, Styled};

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// What the backend can do, for the interface to offer only that.
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    /// How long after sending our messages can be deleted for everyone; `None`: never.
    pub delete_max_age: Option<Duration>,
}

/// A message to send.
#[derive(Debug, Clone)]
pub struct Draft {
    pub text: String,
    pub styles: Vec<Styled>,
    pub attachments: Vec<Attachment>,
    /// The message it replies to, in the same chat.
    pub reply_to: Option<Message>,
    pub forwarded: bool,
}

/// What was downloaded for a message: its attachments, and the images of its link previews
/// in order (`None` for those that failed).
pub type Downloaded = (Vec<Attachment>, Vec<Option<Attachment>>);

/// Something that happened elsewhere: received, or done from another of our devices.
// A few events a second at most: not worth boxing the big ones.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Event {
    /// A chat not known yet; its messages follow.
    Chat(Discussion),
    /// A new message, with the id of the one it quotes. Its images may be downloaded
    /// already; other attachments wait for the user to open them.
    Message {
        chat: ChatId,
        message: Message,
        quote: Option<MessageId>,
    },
    /// A reaction set, or removed with `None`.
    Reaction {
        chat: ChatId,
        target: MessageId,
        author: String,
        mine: bool,
        emoji: Option<String>,
    },
    /// A message deleted for everyone by `author` (only its author can do that).
    Deleted {
        chat: ChatId,
        target: MessageId,
        author: String,
    },
    /// Someone started or stopped typing.
    Typing {
        chat: ChatId,
        author: String,
        name: String,
        started: bool,
    },
    /// Shown in the footer, e.g. a lost connection.
    Status(String),
}

/// A messaging service, as the interface uses it.
///
/// The futures returned are `'static`: they own what they need (clones of the client…),
/// so that downloads can run in the background while the interface goes on. They run on
/// the current thread (in a [`tokio::task::LocalSet`]): nothing needs to be `Send`.
pub trait Backend {
    fn capabilities(&self) -> Capabilities;

    /// Sends `draft` to `chat`, and returns the message as sent.
    fn send(&self, chat: &ChatId, draft: Draft) -> impl Future<Output = Result<Message>> + 'static;

    /// Sets our reaction to `message`, or removes it.
    fn react(
        &self,
        chat: &ChatId,
        message: &Message,
        emoji: &str,
        remove: bool,
    ) -> impl Future<Output = Result<()>> + 'static;

    /// Deletes our `message` for everyone.
    fn delete(
        &self,
        chat: &ChatId,
        message: &Message,
    ) -> impl Future<Output = Result<()>> + 'static;

    /// Downloads the attachments and link preview images of `message`, received without them.
    fn download(
        &self,
        chat: &ChatId,
        message: &Message,
    ) -> impl Future<Output = Result<Downloaded>> + 'static;
}
