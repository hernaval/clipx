use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::models::{ClipboardEvent, JsonRpcRequest, JsonRpcResponse};
use crate::state::{LogLevel, SharedStateHandle};

pub struct IpcClient {
    tx: mpsc::Sender<ClipboardEvent>,
    shared: SharedStateHandle,
}

impl IpcClient {
    pub fn new(tx: mpsc::Sender<ClipboardEvent>, shared: SharedStateHandle) -> Self {
        Self { tx, shared }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        loop {
            match self.connect_and_listen().await {
                Ok(()) => {
                    info!("IPC connection closed, reconnecting...");
                    let mut s = self.shared.lock().unwrap();
                    s.pipe_connected = false;
                    s.log(LogLevel::Warn, "Clipboard bridge disconnected, reconnecting…");
                }
                Err(e) => {
                    warn!("IPC connection error: {e}, retrying in 2s...");
                    {
                        let mut s = self.shared.lock().unwrap();
                        s.pipe_connected = false;
                        s.log(LogLevel::Error, format!("Bridge error: {e}"));
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    async fn connect_and_listen(&self) -> anyhow::Result<()> {
        #[cfg(windows)]
        let pipe = {
            use tokio::net::windows::named_pipe::ClientOptions;
            ClientOptions::new().open(r"\\.\pipe\clipboardsync")?
        };
        #[cfg(not(windows))]
        anyhow::bail!("Named pipe IPC is only supported on Windows");

        info!("Connected to clipboard bridge pipe");
        {
            let mut s = self.shared.lock().unwrap();
            s.pipe_connected = true;
            s.log(LogLevel::Success, "Clipboard bridge connected");
        }

        let mut reader = BufReader::new(pipe);
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break, // EOF — server closed the pipe
                Ok(_) => {}
                Err(e) => return Err(e.into()),
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            debug!("IPC received: {}", &trimmed[..trimmed.len().min(100)]);

            if let Ok(rpc) = serde_json::from_str::<JsonRpcRequest>(trimmed) {
                if rpc.method == "clipboard_event" {
                    if let Some(params) = rpc.params {
                        match serde_json::from_value::<ClipboardEvent>(params) {
                            Ok(evt) => {
                                let preview = evt
                                    .content
                                    .as_deref()
                                    .unwrap_or("")
                                    .chars()
                                    .take(60)
                                    .collect::<String>();
                                let source = if evt.source_app.is_empty() {
                                    String::new()
                                } else {
                                    format!(" [{}]", evt.source_app)
                                };
                                self.shared.lock().unwrap().log(
                                    LogLevel::Info,
                                    format!("Clipboard captured{source}: \"{preview}\""),
                                );
                                let _ = self.tx.send(evt).await;
                            }
                            Err(e) => {
                                warn!("Failed to parse clipboard event: {e}");
                                self.shared.lock().unwrap().log(
                                    LogLevel::Warn,
                                    format!("Parse error: {e}"),
                                );
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub async fn send_rpc(
        method: &str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<JsonRpcResponse> {
        #[cfg(windows)]
        let mut pipe = {
            use tokio::net::windows::named_pipe::ClientOptions;
            ClientOptions::new().open(r"\\.\pipe\clipboardsync")?
        };
        #[cfg(not(windows))]
        anyhow::bail!("Named pipe IPC is only supported on Windows");

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: method.to_string(),
            id: Some(1),
            params,
        };
        let mut json = serde_json::to_string(&req)?;
        json.push('\n');
        pipe.write_all(json.as_bytes()).await?;

        let mut reader = BufReader::new(pipe);
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let resp = serde_json::from_str::<JsonRpcResponse>(&line)?;
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_rpc_serialization() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "get_clipboard".into(),
            id: Some(1),
            params: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("get_clipboard"));

        let parsed: JsonRpcRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.method, "get_clipboard");
    }

    #[test]
    fn test_clipboard_event_deserialization() {
        let json = r#"{"type":"clipboard_event","content":"hello","format":"text","timestamp":"2026-05-29T23:16:00Z"}"#;
        let evt: ClipboardEvent = serde_json::from_str(json).unwrap();
        assert_eq!(evt.event_type, "clipboard_event");
        assert_eq!(evt.content.unwrap(), "hello");
    }
}
