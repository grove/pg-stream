/// stdin / file source (RELAY-29).
/// Reads JSONL from stdin or a file and converts each line to a RelayMessage.
use std::io::BufRead;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::envelope::RelayMessage;
use crate::error::RelayError;

pub struct StdinSource {
    reader: BufReader<tokio::io::Stdin>,
    event_type: String,
    line_buf: String,
}

impl StdinSource {
    pub fn new(event_type: impl Into<String>) -> Self {
        Self {
            reader: BufReader::new(tokio::io::stdin()),
            event_type: event_type.into(),
            line_buf: String::new(),
        }
    }
}

#[async_trait::async_trait]
impl super::Source for StdinSource {
    fn name(&self) -> &str {
        "stdin"
    }

    async fn poll(&mut self, batch_size: i64) -> Result<Vec<RelayMessage>, RelayError> {
        let mut messages = Vec::new();
        for _ in 0..batch_size {
            self.line_buf.clear();
            let n = self
                .reader
                .read_line(&mut self.line_buf)
                .await
                .map_err(RelayError::Io)?;
            if n == 0 {
                // EOF
                break;
            }
            let line = self.line_buf.trim();
            if line.is_empty() {
                continue;
            }
            let payload: serde_json::Value =
                serde_json::from_str(line).map_err(RelayError::Json)?;
            let dedup_key = payload
                .get("dedup_key")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let event_type = payload
                .get("event_type")
                .and_then(|v| v.as_str())
                .unwrap_or(&self.event_type)
                .to_string();
            messages.push(RelayMessage::new_reverse(dedup_key, event_type, payload));
        }
        Ok(messages)
    }

    async fn acknowledge(&mut self, _last_message: &RelayMessage) -> Result<(), RelayError> {
        // stdin is fire-and-forget — no acknowledgement needed.
        Ok(())
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}

/// File source — reads a JSONL file.
pub struct FileSource {
    path: String,
    event_type: String,
    lines: std::collections::VecDeque<String>,
}

impl FileSource {
    pub async fn new(
        path: impl Into<String>,
        event_type: impl Into<String>,
    ) -> Result<Self, RelayError> {
        let path = path.into();
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(RelayError::Io)?;
        let lines = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect();
        Ok(Self {
            path,
            event_type: event_type.into(),
            lines,
        })
    }
}

#[async_trait::async_trait]
impl super::Source for FileSource {
    fn name(&self) -> &str {
        "file"
    }

    async fn poll(&mut self, batch_size: i64) -> Result<Vec<RelayMessage>, RelayError> {
        let mut messages = Vec::new();
        for _ in 0..batch_size {
            let Some(line) = self.lines.pop_front() else {
                break;
            };
            let payload: serde_json::Value =
                serde_json::from_str(&line).map_err(RelayError::Json)?;
            let dedup_key = payload
                .get("dedup_key")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let event_type = payload
                .get("event_type")
                .and_then(|v| v.as_str())
                .unwrap_or(&self.event_type)
                .to_string();
            messages.push(RelayMessage::new_reverse(dedup_key, event_type, payload));
        }
        Ok(messages)
    }

    async fn acknowledge(&mut self, _last_message: &RelayMessage) -> Result<(), RelayError> {
        Ok(())
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Source;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_file_source_jsonl() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            r#"{{"event_type":"order.created","id":1,"dedup_key":"k1"}}"#
        )
        .unwrap();
        writeln!(tmp, r#"{{"event_type":"order.updated","id":2}}"#).unwrap();

        let mut src = FileSource::new(tmp.path().to_str().unwrap(), "default.event")
            .await
            .unwrap();

        let msgs = src.poll(10).await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].subject, "order.created");
        assert_eq!(msgs[0].dedup_key, "k1");
        assert_eq!(msgs[1].subject, "order.updated");
        // Second message has no dedup_key so UUID is generated
        assert!(!msgs[1].dedup_key.is_empty());
    }

    #[tokio::test]
    async fn test_file_source_batch_limit() {
        let mut tmp = NamedTempFile::new().unwrap();
        for i in 0..5 {
            writeln!(tmp, r#"{{"id":{i}}}"#).unwrap();
        }
        let mut src = FileSource::new(tmp.path().to_str().unwrap(), "test")
            .await
            .unwrap();
        let msgs = src.poll(3).await.unwrap();
        assert_eq!(msgs.len(), 3);
        let msgs2 = src.poll(3).await.unwrap();
        assert_eq!(msgs2.len(), 2);
        let msgs3 = src.poll(3).await.unwrap();
        assert!(msgs3.is_empty());
    }

    #[tokio::test]
    async fn test_file_source_empty_file() {
        let tmp = NamedTempFile::new().unwrap();
        let mut src = FileSource::new(tmp.path().to_str().unwrap(), "test")
            .await
            .unwrap();
        let msgs = src.poll(10).await.unwrap();
        assert!(msgs.is_empty());
    }
}
