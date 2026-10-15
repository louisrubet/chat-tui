//! A terminal chat interface, shared by messaging clients (signal-tui,
//! google-messages-tui…): chat list, discussions with thumbnails and link previews,
//! replies, reactions, an fzf-style emoji picker.
//!
//! A client implements [`Backend`] for its service, loads the chats, then calls [`run`]
//! with the stream of [`Event`]s it receives.

pub mod backend;
pub mod data;
pub mod editor;
pub mod fuzzy;
pub mod graphics;
pub mod qr;
mod run;
pub mod settings;
pub mod store;
pub mod tui;
pub mod ui;

pub use backend::{Backend, Capabilities, Downloaded, Draft, Event};
pub use run::{Config, run};
