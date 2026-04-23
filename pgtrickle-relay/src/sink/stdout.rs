/// stdout / file sink (RELAY-5).
/// Writes RelayMessages as JSONL to stdout or a file.
use std::io::Write;

use crate::envelope::RelayMessage;
use crate::error::RelayError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdoutFormat {
    Jsonl,
    JsonPretty,
}

pub struct StdoutSink {
    format: StdoutFormat,
}

impl StdoutSink {
    pub fn new(format: StdoutFormat) -> Self {
        Self { format }
    }
}

#[async_trait::async_trait]
impl super::Sink for StdoutSink {
    fn name(&self) -> &str {
        "stdout"
    }

    async fn publish(&mut self, messages: &[RelayMessage]) -> Result<(), RelayError> {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        for msg in messages {
            match self.format {
                StdoutFormat::Jsonl => {
                    let line = serde_json::to_string(msg).map_err(RelayError::Json)?;
                    writeln!(handle, "{line}").map_err(RelayError::Io)?;
                }
                StdoutFormat::JsonPretty => {
                    let line = serde_json::to_string_pretty(msg).map_err(RelayError::Json)?;
                    writeln!(handle, "{line}").map_err(RelayError::Io)?;
                }
            }
        }
        Ok(())
    }

    async fn is_healthy(&mut self) -> bool {
        true
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        Ok(())
    }
}

/// File sink — writes JSONL to a file.
pub struct FileSink {
    path: String,
    file: tokio::fs::File,
    format: StdoutFormat,
}

impl FileSink {
    pub async fn new(path: impl Into<String>, format: StdoutFormat) -> Result<Self, RelayError> {
        let path = path.into();
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(RelayError::Io)?;
        Ok(Self { path, file, format })
    }
}

#[async_trait::async_trait]
impl super::Sink for FileSink {
    fn name(&self) -> &str {
        "file"
    }

    async fn publish(&mut self, messages: &[RelayMessage]) -> Result<(), RelayError> {
        use tokio::io::AsyncWriteExt;
        for msg in messages {
            let line = match self.format {
                StdoutFormat::Jsonl => serde_json::to_string(msg).map_err(RelayError::Json)?,
                StdoutFormat::JsonPretty => {
                    serde_json::to_string_pretty(msg).map_err(RelayError::Json)?
                }
            };
            self.file
                .write_all(format!("{line}\n").as_bytes())
                .await
                .map_err(RelayError::Io)?;
        }
        self.file.flush().await.map_err(RelayError::Io)?;
        Ok(())
    }

    async fn is_healthy(&mut self) -> bool {
        true
    }

    async fn close(&mut self) -> Result<(), RelayError> {
        use tokio::io::AsyncWriteExt;
        let _ = self.file.flush().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::RelayMessage;
    use crate::sink::Sink;

    #[tokio::test]
    async fn test_stdout_sink_jsonl_no_panic() {
        let mut sink = StdoutSink::new(StdoutFormat::Jsonl);
        let msg = RelayMessage::new_forward(
            "outbox_orders",
            1,
            0,
            "insert",
            serde_json::json!({"id": 1}),
            false,
            None,
            "orders.insert",
        );
        sink.publish(&[msg]).await.unwrap();
    }

    #[tokio::test]
    async fn test_file_sink_writes_jsonl() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let mut sink = FileSink::new(&path, StdoutFormat::Jsonl).await.unwrap();

        let msg1 = RelayMessage::new_forward(
            "outbox_orders",
            1,
            0,
            "insert",
            serde_json::json!({"id": 1}),
            false,
            None,
            "orders.insert",
        );
        let msg2 = RelayMessage::new_forward(
            "outbox_orders",
            1,
            1,
            "delete",
            serde_json::json!({"id": 0}),
            false,
            None,
            "orders.delete",
        );
        sink.publish(&[msg1, msg2]).await.unwrap();
        sink.close().await.unwrap();

        let content = tokio::fs::read_to_string(&path).await.unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        let decoded: RelayMessage = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(decoded.op, "insert");
    }

    #[tokio::test]
    async fn test_file_sink_pretty_format() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let mut sink = FileSink::new(&path, StdoutFormat::JsonPretty)
            .await
            .unwrap();
        let msg = RelayMessage::new_reverse("key1", "order.created", serde_json::json!({"id": 42}));
        sink.publish(&[msg]).await.unwrap();
        sink.close().await.unwrap();
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        // Pretty format has multiple lines per object.
        assert!(content.lines().count() > 1);
        assert!(content.contains("\"dedup_key\""));
    }
}
