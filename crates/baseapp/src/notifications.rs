//! Local desktop notification preferences; room unread state is independent.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationPreferences {
    pub enabled: bool,
    pub direct_messages: bool,
    pub room_messages: bool,
    pub mentions: bool,
    pub thread_replies: bool,
    pub calls: bool,
    pub show_preview: bool,
    pub attention: bool,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            enabled: true,
            direct_messages: true,
            room_messages: true,
            mentions: true,
            thread_replies: true,
            calls: true,
            show_preview: true,
            attention: true,
        }
    }
}

impl NotificationPreferences {
    pub fn allows_message(self, direct: bool, thread: bool, mentioned: bool) -> bool {
        let category = if thread {
            self.thread_replies
        } else if direct {
            self.direct_messages
        } else {
            self.room_messages
        };
        self.enabled && (category || (mentioned && self.mentions))
    }

    pub fn allows_call(self) -> bool {
        self.enabled && self.calls
    }

    pub fn preview(self, body: String) -> String {
        if self.show_preview {
            body
        } else {
            "New message".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_switch_blocks_all_alert_categories() {
        let prefs = NotificationPreferences {
            enabled: false,
            ..Default::default()
        };
        for direct in [false, true] {
            for thread in [false, true] {
                for mentioned in [false, true] {
                    assert!(!prefs.allows_message(direct, thread, mentioned));
                }
            }
        }
        assert!(!prefs.allows_call());
    }

    #[test]
    fn categories_and_mentions_are_independent() {
        let mut prefs = NotificationPreferences {
            direct_messages: false,
            room_messages: false,
            thread_replies: false,
            calls: false,
            ..Default::default()
        };
        assert!(!prefs.allows_message(true, false, false));
        assert!(!prefs.allows_message(false, false, false));
        assert!(!prefs.allows_message(false, true, false));
        assert!(prefs.allows_message(false, false, true));
        assert!(prefs.allows_message(true, true, true));
        assert!(!prefs.allows_call());
        prefs.mentions = false;
        assert!(!prefs.allows_message(false, false, true));
        prefs.direct_messages = true;
        assert!(prefs.allows_message(true, false, false));
        assert!(!prefs.allows_message(false, false, false));
        assert!(!prefs.allows_message(true, true, false));
        prefs.thread_replies = true;
        assert!(prefs.allows_message(false, true, false));
        prefs.room_messages = true;
        assert!(prefs.allows_message(false, false, true));
    }

    #[test]
    fn missing_fields_keep_defaults() {
        let prefs: NotificationPreferences = serde_json::from_str(r#"{"enabled":false}"#).unwrap();
        assert!(!prefs.enabled);
        assert!(prefs.calls && prefs.show_preview && prefs.direct_messages);
    }

    #[test]
    fn notifications_can_hide_message_content() {
        let prefs = NotificationPreferences {
            show_preview: false,
            ..Default::default()
        };
        assert_eq!(prefs.preview("Private message text".into()), "New message");
        assert_eq!(
            NotificationPreferences::default().preview("A message".into()),
            "A message"
        );
    }
}
