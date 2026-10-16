# Changelog

## v1.1.2

- No panic drawing the chat list or the parameters in a terminal of no rows.

## v1.1.1

- Unread counts above 20 in digits (`21`, `99+`): circled ㉑ and up took two columns and stuck out on the left.
- History arriving after a chat was opened: its images are fetched too.

## v1.1.0

- `Event::History`: earlier messages of a chat, merged with those shown (known ones replaced, send order kept), without notifying. Selection, quotes and the reply being written keep pointing to their message.

## v1.0.0

Stable API: same as v0.1.0, now used by signal-tui.

## v0.1.0

First release, extracted from signal-tui v1.3.1.

- `Backend` trait: `capabilities`, `send`, `react`, `delete`, `download`.
- `Event` stream: `Chat`, `Message`, `Reaction`, `Deleted`, `Typing`, `Status`.
- `run(backend, chats, events, config)`, `Config::new(name)`.
- Chats and messages identified by strings (`ChatId`, `MessageId`), send time in `Message::sent_ms`.
- Settings, pinned chats and read marks kept by chat-tui.
