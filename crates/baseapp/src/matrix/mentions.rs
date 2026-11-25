use matrix_sdk::ruma::events::{
    Mentions,
    room::message::{MessageType, RoomMessageEventContent},
};
use matrix_sdk::ruma::{MatrixToUri, OwnedUserId, matrix_uri::MatrixId};
use ruma::html::{Html, NodeData, NodeRef};
use std::ops::Range;

#[derive(Clone)]
struct Token {
    range: Range<usize>,
    user: OwnedUserId,
}

#[derive(Default)]
pub(super) struct Draft {
    text: String,
    tokens: Vec<Token>,
    pub selected: usize,
    pub dismissed: Option<(String, usize)>,
}

impl Draft {
    pub fn edit(&mut self, text: &str) {
        if self.text == text {
            return;
        }
        let prefix = self
            .text
            .chars()
            .zip(text.chars())
            .take_while(|(a, b)| a == b)
            .map(|(c, _)| c.len_utf8())
            .sum::<usize>();
        let suffix = self.text[prefix..]
            .chars()
            .rev()
            .zip(text[prefix..].chars().rev())
            .take_while(|(a, b)| a == b)
            .map(|(c, _)| c.len_utf8())
            .sum::<usize>();
        let old_end = self.text.len() - suffix;
        let new_end = text.len() - suffix;
        self.tokens.retain_mut(|token| {
            if token.range.end <= prefix {
                true
            } else if token.range.start >= old_end {
                token.range =
                    (token.range.start - old_end + new_end)..(token.range.end - old_end + new_end);
                true
            } else {
                false
            }
        });
        self.text = text.to_owned();
        self.selected = 0;
        self.dismissed = None;
    }

    pub fn insert(
        &mut self,
        old: &str,
        range: Range<usize>,
        label: &str,
        user: OwnedUserId,
    ) -> String {
        self.edit(old);
        let mut next = old.to_owned();
        next.replace_range(range.clone(), &format!("{label} "));
        self.edit(&next);
        self.tokens.push(Token {
            range: range.start..range.start + label.len(),
            user,
        });
        self.tokens.sort_by_key(|token| token.range.start);
        next
    }

    pub fn markdown(&self, text: &str) -> String {
        if text != self.text {
            return text.to_owned();
        }
        let mut result = text.to_owned();
        for token in self.tokens.iter().rev() {
            let label = text[token.range.clone()]
                .chars()
                .flat_map(|c| {
                    if "\\`*_{}[]<>()!".contains(c) {
                        vec!['\\', c]
                    } else {
                        vec![c]
                    }
                })
                .collect::<String>();
            result.replace_range(
                token.range.clone(),
                &format!("[{label}]({})", token.user.matrix_to_uri()),
            );
        }
        result
    }
}

/// Only complete a standalone @ token at the caret, never email addresses or selected text.
pub(super) fn query(text: &str, selection: Range<usize>) -> Option<(Range<usize>, &str)> {
    if !selection.is_empty() || !text.is_char_boundary(selection.start) {
        return None;
    }
    let head = text.get(..selection.start)?;
    let at = head.rfind('@')?;
    if head[..at]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace() && !"([{>\"'".contains(c))
    {
        return None;
    }
    let query = &head[at + 1..];
    if query
        .chars()
        .any(|c| c.is_whitespace() || "[]()<>`".contains(c))
    {
        return None;
    }
    Some((at..selection.start, query))
}

/// Matrix pings are explicit metadata; a visible link alone doesn't notify anyone.
pub(super) fn content(body: &str) -> RoomMessageEventContent {
    fn visit(node: &NodeRef, users: &mut std::collections::BTreeSet<OwnedUserId>) {
        if let NodeData::Element(element) = node.data() {
            let name: &str = &element.name.local;
            if ["code", "pre", "mx-reply"].contains(&name) {
                return;
            }
            if name == "a" {
                for attr in element.attrs.borrow().iter() {
                    if &*attr.name.local == "href" {
                        if let Ok(uri) = MatrixToUri::parse(&attr.value) {
                            if let MatrixId::User(user) = uri.id() {
                                users.insert(user.clone());
                            }
                        }
                    }
                }
            }
        }
        for child in node.children() {
            visit(&child, users);
        }
    }
    let content = RoomMessageEventContent::text_markdown(body);
    let mut users = std::collections::BTreeSet::new();
    if let MessageType::Text(text) = &content.msgtype {
        if let Some(formatted) = &text.formatted {
            for node in Html::parse(&formatted.body).children() {
                visit(&node, &mut users);
            }
        }
    }
    content.add_mentions(Mentions::with_user_ids(users))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn user() -> OwnedUserId {
        "@ala:example.org".parse().unwrap()
    }
    #[test]
    fn completion_preserves_unicode_and_surrounding_text_and_tracks_edits() {
        let mut draft = Draft::default();
        let text = "Hej @al, co słychać?";
        let next = draft.insert(text, 4..7, "@Ala 🐈", user());
        assert_eq!(next, "Hej @Ala 🐈 , co słychać?");
        assert!(
            draft
                .markdown(&next)
                .contains("matrix.to/#/@ala:example.org")
        );
        let edited = format!("Cześć! {next}");
        draft.edit(&edited);
        assert!(
            draft
                .markdown(&edited)
                .contains("matrix.to/#/@ala:example.org")
        );
        let deleted = edited.replace("@Ala 🐈", "@Ktoś");
        draft.edit(&deleted);
        assert_eq!(draft.markdown(&deleted), deleted);
    }
    #[test]
    fn query_requires_a_standalone_at_and_a_collapsed_selection() {
        assert_eq!(query("Cześć @al", 11..11).unwrap().1, "al");
        assert!(query("mail@host", 9..9).is_none());
        assert!(query("@ala tekst", 10..10).is_none());
        assert!(query("@ala", 0..4).is_none());
        assert!(query("@ą", 2..2).is_none());
    }
    #[test]
    fn matrix_mentions_are_deduplicated_and_code_and_external_links_dont_ping() {
        let message = content(
            "[@Ala](https://matrix.to/#/@ala:example.org) [again](https://matrix.to/#/%40ala%3Aexample.org) `[@Bob](https://matrix.to/#/@bob:example.org)` [fake](https://evil.example/#/@eve:example.org)",
        );
        let mentions = message.mentions.unwrap();
        assert_eq!(
            mentions.user_ids.into_iter().collect::<Vec<_>>(),
            vec![user()]
        );
        assert!(!mentions.room);
        assert!(
            content("ordinary @text email@example.org")
                .mentions
                .unwrap()
                .user_ids
                .is_empty()
        );
    }
}
