# chat-tui

The terminal chat interface of [signal-tui](https://github.com/louisrubet/signal-tui) and google-messages-tui, as a library: each client brings its service, chat-tui does the rest.

- **Chat list** with pinned chats and unread counts
- **Discussions**: replies, forwards, deletion, `**bold**` `*italic*` `~strikethrough~`
- **Images** as thumbnails (kitty graphics protocol), pasted with `Ctrl+V`, opened with `o`
- **Link previews**, desktop notifications, typing indicators
- **Emojis**: reactions, fuzzy `:shortcode` picker (fzf style)

## Use

```toml
[dependencies]
chat-tui = { git = "https://github.com/louisrubet/chat-tui", tag = "v1.1.2" }
```

Implement [`Backend`](src/backend.rs) for your service, load the chats, then run:

```rust
struct MyService { /* client */ }

impl chat_tui::Backend for MyService {
    fn capabilities(&self) -> Capabilities { … }
    fn send(&self, chat: &ChatId, draft: Draft) -> impl Future<Output = Result<Message>> + 'static { … }
    fn react(…) / fn delete(…) / fn download(…)
}

let config = chat_tui::Config::new("my-tui")?;          // ~/.config/my-tui, ~/.local/share/my-tui
chat_tui::run(service, chats, events, config).await?;   // in a tokio LocalSet
```

- `chats`: the `Discussion`s to show, with their messages.
- `events`: a `Stream` of `Event`s: messages received, reactions, deletions, typing, new chats.
- The `Backend` futures are `'static` and need not be `Send`: clone your client into them.

chat-tui keeps the settings, the pinned chats and the read marks; your client keeps its account and store.

## Terminal

Any terminal works; thumbnails need the kitty graphics protocol with Unicode placeholders: [Ghostty](https://ghostty.org), [kitty](https://sw.kovidgoyal.net/kitty/), [Rio](https://rioterm.com). See [signal-tui](https://github.com/louisrubet/signal-tui#terminal).

## Versions

Tagged `vX.Y.Z` (semver), clients pin a tag: a major bump breaks the API, see the [changelog](CHANGELOG.md).

To work on chat-tui and a client together, point the client at a checkout, without committing it:

```toml
[patch."https://github.com/louisrubet/chat-tui"]
chat-tui = { path = "../chat-tui" }
```

## Dev

```sh
cargo run --example mock_tui      # fake chats, no network (`-- --open`: first chat)
cargo test
```

Extracted from signal-tui v1.3.1, with its history.

## License

[AGPL-3.0-only](LICENSE).
