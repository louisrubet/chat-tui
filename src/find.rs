//! Finding chats by their name, and messages by their text, ignoring case and accents.

use crate::data::{Discussion, MessageId};

/// Messages found at most: the most recent ones.
pub const MAX_MESSAGES: usize = 500;

/// What a search found.
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    /// A chat whose name matches.
    Chat(usize),
    /// A message of a chat, whose text matches: by its id, messages may come before it.
    Message(usize, MessageId),
}

/// The chats whose name holds `query` (most recently active first), then the messages whose
/// text holds it (most recent first, at most [`MAX_MESSAGES`]); hidden chats left out.
pub fn find(discussions: &[Discussion], query: &str) -> Vec<Found> {
    let query = fold(query);
    if query.trim().is_empty() {
        return Vec::new();
    }
    let shown = |d: &&Discussion| !d.hidden;
    let mut chats: Vec<usize> = (0..discussions.len())
        .filter(|&i| shown(&&discussions[i]) && fold(&discussions[i].title).contains(&query))
        .collect();
    chats.sort_by_key(|&i| std::cmp::Reverse(last_ms(&discussions[i])));
    let mut messages: Vec<(u64, usize, &MessageId)> = Vec::new();
    for (i, d) in discussions.iter().enumerate().filter(|(_, d)| shown(d)) {
        for message in &d.messages {
            if !message.deleted && fold(&message.text).contains(&query) {
                messages.push((message.sent_ms, i, &message.id));
            }
        }
    }
    messages.sort_by_key(|&(sent, ..)| std::cmp::Reverse(sent));
    messages.truncate(MAX_MESSAGES);
    chats
        .into_iter()
        .map(Found::Chat)
        .chain(
            messages
                .into_iter()
                .map(|(_, i, m)| Found::Message(i, m.clone())),
        )
        .collect()
}

fn last_ms(discussion: &Discussion) -> u64 {
    discussion.messages.last().map_or(0, |m| m.sent_ms)
}

/// `text` in lowercase without accents, one char for each char of `text`: positions in
/// it are positions in `text`.
pub fn fold(text: &str) -> String {
    text.chars().map(fold_char).collect()
}

fn fold_char(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'č' => 'c',
        'ď' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' => 'i',
        'ł' | 'ľ' => 'l',
        'ñ' | 'ń' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => 'o',
        'ř' => 'r',
        'ś' | 'š' | 'ş' => 's',
        'ť' | 'ţ' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => 'u',
        'ý' | 'ÿ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        // Typed apostrophes find the typographic ones.
        '\u{2019}' => '\'',
        c => c,
    }
}

/// The part of `text` around the first match of `query` (on one line), at most `width`
/// chars from `before` chars ahead of it, with the byte range of the match in it.
pub fn snippet(
    text: &str,
    query: &str,
    before: usize,
    width: usize,
) -> (String, Option<(usize, usize)>) {
    let chars: Vec<char> = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let folded: Vec<char> = text.chars().map(fold_char).collect();
    let query: Vec<char> = fold(query).chars().collect();
    let at = (!query.is_empty())
        .then(|| {
            folded
                .windows(query.len())
                .position(|w| w == query.as_slice())
        })
        .flatten();
    let start = at.map_or(0, |at| at.saturating_sub(before));
    let mut out = String::new();
    let mut range = None;
    if start > 0 {
        out.push('\u{2026}');
    }
    for (i, &c) in chars.iter().enumerate().skip(start).take(width) {
        if Some(i) == at {
            range = Some((out.len(), out.len()));
        }
        out.push(c);
        if let Some(at) = at
            && i + 1 == at + query.len()
            && let Some((s, _)) = range
        {
            range = Some((s, out.len()));
        }
    }
    (out, range)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Message;

    fn chat(title: &str, texts: &[(&str, u64)]) -> Discussion {
        let mut d = Discussion::new(title.to_lowercase(), title);
        for (i, (text, sent)) in texts.iter().enumerate() {
            let mut m = Message::mine(format!("{title}{i}"), text.to_string(), None, false);
            m.sent_ms = *sent;
            d.messages.push(m);
        }
        d
    }

    #[test]
    fn names_then_messages_without_case_nor_accents() {
        let mut chats = vec![
            chat("Hélène", &[("À demain", 10)]),
            chat("Bob", &[("Il faut voir HELENE", 30), ("rien", 40)]),
            chat("Helena", &[("salut", 50)]),
        ];
        assert_eq!(
            find(&chats, "helen"),
            [
                Found::Chat(2),
                Found::Chat(0),
                Found::Message(1, "Bob0".into())
            ]
        );
        assert_eq!(
            find(&chats, "a DEMAIN"),
            [Found::Message(0, "Hélène0".into())]
        );
        assert!(find(&chats, "  ").is_empty());
        chats[2].hidden = true;
        assert_eq!(find(&chats, "helen")[0], Found::Chat(0), "hidden: left out");
    }

    #[test]
    fn the_snippet_shows_the_match() {
        let (text, range) = snippet("Rendez-vous\ndevant l'école à 8h", "ECOLE", 5, 20);
        assert_eq!(text, "\u{2026}nt l'école à 8h");
        let (s, e) = range.unwrap();
        assert_eq!(&text[s..e], "école");
        assert_eq!(snippet("court", "x", 5, 20), ("court".to_string(), None));
    }
}
