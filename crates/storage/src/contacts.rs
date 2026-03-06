use blockcell_core::Paths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::debug;

/// A single known contact/chat entry for a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelContact {
    pub channel: String,
    pub chat_id: String,
    pub sender_id: String,
    /// Human-readable name (nick, username, group title, etc.)
    pub name: String,
    /// "private" or "group"
    pub chat_type: String,
    /// ISO-8601 timestamp of last activity
    pub last_active: String,
}

/// Persistent registry of known channel contacts.
/// Stored as a flat JSON array at `~/.blockcell/channel_contacts.json`.
#[derive(Debug, Clone)]
pub struct ChannelContacts {
    paths: Paths,
}

impl ChannelContacts {
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }

    fn file_path(&self) -> std::path::PathBuf {
        self.paths.channel_contacts_file()
    }

    /// Load all contacts from disk.
    pub fn load(&self) -> Vec<ChannelContact> {
        let path = self.file_path();
        if !path.exists() {
            return vec![];
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Save all contacts to disk.
    fn save(&self, contacts: &[ChannelContact]) {
        let path = self.file_path();
        let _ = std::fs::write(
            &path,
            serde_json::to_string_pretty(contacts).unwrap_or_default(),
        );
    }

    /// Upsert a contact: if a matching (channel, chat_id) entry exists, update it;
    /// otherwise insert a new entry.
    pub fn upsert(&self, contact: ChannelContact) {
        let mut contacts = self.load();
        let existing = contacts
            .iter_mut()
            .find(|c| c.channel == contact.channel && c.chat_id == contact.chat_id);
        if let Some(entry) = existing {
            // Update name only if the new one is non-empty
            if !contact.name.is_empty() {
                entry.name = contact.name.clone();
            }
            entry.sender_id = contact.sender_id.clone();
            entry.chat_type = contact.chat_type.clone();
            entry.last_active = contact.last_active.clone();
            debug!(
                channel = %contact.channel,
                chat_id = %contact.chat_id,
                name = %entry.name,
                "Updated existing channel contact"
            );
        } else {
            debug!(
                channel = %contact.channel,
                chat_id = %contact.chat_id,
                name = %contact.name,
                "Inserted new channel contact"
            );
            contacts.push(contact);
        }
        self.save(&contacts);
    }

    /// Look up contacts by channel and name (case-insensitive substring match).
    /// Returns all matches sorted by last_active descending.
    pub fn lookup(&self, channel: &str, name: &str) -> Vec<ChannelContact> {
        let name_lower = name.to_lowercase();
        let mut matches: Vec<ChannelContact> = self
            .load()
            .into_iter()
            .filter(|c| c.channel == channel && c.name.to_lowercase().contains(&name_lower))
            .collect();
        matches.sort_by(|a, b| b.last_active.cmp(&a.last_active));
        matches
    }

    /// Look up contacts by channel only. Returns all contacts for that channel.
    pub fn list_by_channel(&self, channel: &str) -> Vec<ChannelContact> {
        self.load()
            .into_iter()
            .filter(|c| c.channel == channel)
            .collect()
    }

    /// Build a summary of known contacts grouped by channel.
    /// Useful for injecting into system prompts.
    pub fn summary(&self) -> HashMap<String, Vec<String>> {
        let contacts = self.load();
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for c in contacts {
            let label = if c.name.is_empty() {
                format!("{} ({})", c.chat_id, c.chat_type)
            } else {
                format!("{} → {} ({})", c.name, c.chat_id, c.chat_type)
            };
            map.entry(c.channel.clone()).or_default().push(label);
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_contacts() -> (ChannelContacts, TempDir) {
        let dir = TempDir::new().unwrap();
        let paths = Paths::with_base(dir.path().to_path_buf());
        (ChannelContacts::new(paths), dir)
    }

    #[test]
    fn test_upsert_and_load() {
        let (store, _dir) = test_contacts();
        assert!(store.load().is_empty());

        store.upsert(ChannelContact {
            channel: "dingtalk".into(),
            chat_id: "user123".into(),
            sender_id: "user123".into(),
            name: "张三".into(),
            chat_type: "private".into(),
            last_active: "2025-01-01T00:00:00Z".into(),
        });

        let all = store.load();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "张三");
        assert_eq!(all[0].chat_id, "user123");
    }

    #[test]
    fn test_upsert_updates_existing() {
        let (store, _dir) = test_contacts();
        store.upsert(ChannelContact {
            channel: "dingtalk".into(),
            chat_id: "user123".into(),
            sender_id: "user123".into(),
            name: "张三".into(),
            chat_type: "private".into(),
            last_active: "2025-01-01T00:00:00Z".into(),
        });
        store.upsert(ChannelContact {
            channel: "dingtalk".into(),
            chat_id: "user123".into(),
            sender_id: "user123".into(),
            name: "张三丰".into(),
            chat_type: "private".into(),
            last_active: "2025-01-02T00:00:00Z".into(),
        });

        let all = store.load();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "张三丰");
        assert_eq!(all[0].last_active, "2025-01-02T00:00:00Z");
    }

    #[test]
    fn test_lookup_by_name() {
        let (store, _dir) = test_contacts();
        store.upsert(ChannelContact {
            channel: "dingtalk".into(),
            chat_id: "u1".into(),
            sender_id: "u1".into(),
            name: "Alice".into(),
            chat_type: "private".into(),
            last_active: "2025-01-01T00:00:00Z".into(),
        });
        store.upsert(ChannelContact {
            channel: "dingtalk".into(),
            chat_id: "u2".into(),
            sender_id: "u2".into(),
            name: "Bob".into(),
            chat_type: "private".into(),
            last_active: "2025-01-02T00:00:00Z".into(),
        });
        store.upsert(ChannelContact {
            channel: "telegram".into(),
            chat_id: "t1".into(),
            sender_id: "t1".into(),
            name: "Alice T".into(),
            chat_type: "private".into(),
            last_active: "2025-01-03T00:00:00Z".into(),
        });

        let results = store.lookup("dingtalk", "alice");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chat_id, "u1");

        let results = store.lookup("dingtalk", "bob");
        assert_eq!(results.len(), 1);

        let results = store.lookup("telegram", "alice");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chat_id, "t1");

        let results = store.lookup("dingtalk", "nobody");
        assert!(results.is_empty());
    }

    #[test]
    fn test_list_by_channel() {
        let (store, _dir) = test_contacts();
        store.upsert(ChannelContact {
            channel: "dingtalk".into(),
            chat_id: "u1".into(),
            sender_id: "u1".into(),
            name: "A".into(),
            chat_type: "private".into(),
            last_active: "2025-01-01T00:00:00Z".into(),
        });
        store.upsert(ChannelContact {
            channel: "telegram".into(),
            chat_id: "t1".into(),
            sender_id: "t1".into(),
            name: "B".into(),
            chat_type: "private".into(),
            last_active: "2025-01-01T00:00:00Z".into(),
        });

        assert_eq!(store.list_by_channel("dingtalk").len(), 1);
        assert_eq!(store.list_by_channel("telegram").len(), 1);
        assert_eq!(store.list_by_channel("slack").len(), 0);
    }

    #[test]
    fn test_summary() {
        let (store, _dir) = test_contacts();
        store.upsert(ChannelContact {
            channel: "dingtalk".into(),
            chat_id: "u1".into(),
            sender_id: "u1".into(),
            name: "张三".into(),
            chat_type: "private".into(),
            last_active: "2025-01-01T00:00:00Z".into(),
        });

        let summary = store.summary();
        assert!(summary.contains_key("dingtalk"));
        assert_eq!(summary["dingtalk"].len(), 1);
        assert!(summary["dingtalk"][0].contains("张三"));
    }
}
