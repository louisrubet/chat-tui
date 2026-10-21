//! Choosing whom to start a chat with: contacts by name or number, or numbers typed.

use crate::find::fold;

/// Someone of the address book, at one of their numbers; or a number typed (no name).
#[derive(Debug, Clone, PartialEq)]
pub struct Contact {
    pub name: String,
    pub number: String,
}

impl Contact {
    /// As shown among the recipients: the name, else the number.
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.number
        } else {
            &self.name
        }
    }
}

/// The phone number `text` stands for: its digits, after a `+` if it starts with one;
/// `None` if it is not one (letters, fewer than 3 digits).
pub fn number(text: &str) -> Option<String> {
    let text = text.trim();
    if !text
        .chars()
        .all(|c| c.is_ascii_digit() || " +-.()/".contains(c))
    {
        return None;
    }
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 3 {
        return None;
    }
    Some(if text.starts_with('+') {
        format!("+{digits}")
    } else {
        digits
    })
}

/// The contacts whose name or number holds `query`, ignoring case, accents and how
/// numbers are written (a national `0` matching the international form), by name; all of
/// them for an empty query.
pub fn matching<'a>(contacts: &'a [Contact], query: &str) -> Vec<&'a Contact> {
    let name = fold(query.trim());
    let digits = number(query).map(|n| n.trim_start_matches('+').to_string());
    let mut found: Vec<&Contact> = contacts
        .iter()
        .filter(|c| match &digits {
            Some(digits) => {
                let number: String = c.number.chars().filter(char::is_ascii_digit).collect();
                number.contains(digits.as_str())
                    || digits
                        .strip_prefix('0')
                        .is_some_and(|national| national.len() >= 3 && number.contains(national))
            }
            None => fold(&c.name).contains(&name),
        })
        .collect();
    found.sort_by_cached_key(|c| fold(&c.name));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(name: &str, number: &str) -> Contact {
        Contact {
            name: name.to_string(),
            number: number.to_string(),
        }
    }

    #[test]
    fn numbers_typed() {
        assert_eq!(number("06 12-34.56 78"), Some("0612345678".to_string()));
        assert_eq!(
            number(" +33 6 12 34 56 78"),
            Some("+33612345678".to_string())
        );
        assert_eq!(number("12"), None);
        assert_eq!(number("Hélène"), None);
        assert_eq!(number(""), None);
    }

    #[test]
    fn contacts_by_name_or_number() {
        let contacts = [
            contact("Zoé Martin", "+33612345678"),
            contact("Hélène", "+33698765432"),
            contact("Bob", "0611111111"),
        ];
        let names = |query| -> Vec<&str> {
            matching(&contacts, query)
                .iter()
                .map(|c| c.name.as_str())
                .collect()
        };
        assert_eq!(names("helen"), ["Hélène"]);
        assert_eq!(names("MAR"), ["Zoé Martin"]);
        assert_eq!(names("06 12 34"), ["Zoé Martin"], "national form");
        assert_eq!(names("+336987"), ["Hélène"]);
        assert_eq!(names("0611"), ["Bob"]);
        assert_eq!(names(""), ["Bob", "Hélène", "Zoé Martin"]);
    }
}
