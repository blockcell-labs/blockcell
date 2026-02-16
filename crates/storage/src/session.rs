use blockcell_core::types::ChatMessage;
use blockcell_core::{Paths, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use tracing::debug;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "_type")]
enum SessionLine {
    #[serde(rename = "metadata")]
    Metadata {
        created_at: String,
        updated_at: String,
        #[serde(default)]
        metadata: serde_json::Value,
    },
    #[serde(untagged)]
    Message(ChatMessage),
}

pub struct SessionStore {
    paths: Paths,
}

impl SessionStore {
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }

    pub fn load(&self, session_key: &str) -> Result<Vec<ChatMessage>> {
        let path = self.paths.session_file(session_key);
        
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&path)?;
        let reader = BufReader::new(file);
        let mut messages = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<SessionLine>(&line) {
                Ok(SessionLine::Message(msg)) => {
                    messages.push(msg);
                }
                Ok(SessionLine::Metadata { .. }) => {
                    // Skip metadata line
                }
                Err(e) => {
                    debug!(error = %e, "Failed to parse session line, skipping");
                }
            }
        }

        Ok(messages)
    }

    pub fn save(&self, session_key: &str, messages: &[ChatMessage]) -> Result<()> {
        let path = self.paths.session_file(session_key);
        
        // Ensure directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let now = chrono::Utc::now().to_rfc3339();
        
        let mut file = File::create(&path)?;

        // Write metadata
        let metadata = SessionLine::Metadata {
            created_at: now.clone(),
            updated_at: now,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        };
        writeln!(file, "{}", serde_json::to_string(&metadata)?)?;

        // Write messages
        for msg in messages {
            writeln!(file, "{}", serde_json::to_string(msg)?)?;
        }

        Ok(())
    }

    pub fn append(&self, session_key: &str, message: &ChatMessage) -> Result<()> {
        let path = self.paths.session_file(session_key);
        
        // Ensure directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Create file with metadata if it doesn't exist
        if !path.exists() {
            let now = chrono::Utc::now().to_rfc3339();
            let mut file = File::create(&path)?;
            let metadata = SessionLine::Metadata {
                created_at: now.clone(),
                updated_at: now,
                metadata: serde_json::Value::Object(serde_json::Map::new()),
            };
            writeln!(file, "{}", serde_json::to_string(&metadata)?)?;
        }

        // Append message
        let mut file = OpenOptions::new().append(true).open(&path)?;
        writeln!(file, "{}", serde_json::to_string(message)?)?;

        Ok(())
    }
}
