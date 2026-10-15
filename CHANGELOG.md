# Changelog

## v1.0.0

Stable API: same as v0.1.0, now used by signal-tui.

## v0.1.0

First release, extracted from signal-tui v1.3.1.

- `Backend` trait: `capabilities`, `send`, `react`, `delete`, `download`.
- `Event` stream: `Chat`, `Message`, `Reaction`, `Deleted`, `Typing`, `Status`.
- `run(backend, chats, events, config)`, `Config::new(name)`.
- Chats and messages identified by strings (`ChatId`, `MessageId`), send time in `Message::sent_ms`.
- Settings, pinned chats and read marks kept by chat-tui.
