//! Minimal LSP client over stdio: spawn a language server, initialize,
//! open documents, collect publishDiagnostics.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};
use tokio::time::{Duration, timeout};

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("server exited or channel closed")]
    Closed,
    #[error("timeout waiting for {0}")]
    Timeout(String),
    #[error("bad frame: {0}")]
    Frame(String),
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Diag {
    pub file: PathBuf,
    pub line: u32,
    pub severity: String,
    pub message: String,
}

pub fn encode_frame(msg: &Value) -> Vec<u8> {
    let body = msg.to_string();
    format!("Content-Length: {}\r\n\r\n{}", body.len(), body).into_bytes()
}

pub async fn read_frame<R: AsyncBufReadExt + AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<Value, LspError> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(LspError::Closed);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            content_length = Some(
                rest.trim()
                    .parse()
                    .map_err(|_| LspError::Frame(format!("bad length: {rest}")))?,
            );
        }
    }
    let len = content_length.ok_or_else(|| LspError::Frame("missing Content-Length".into()))?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;
type DiagStore = Arc<Mutex<HashMap<PathBuf, Vec<Diag>>>>;

pub struct LspClient {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    diags: DiagStore,
    next_id: AtomicI64,
    root: PathBuf,
}

fn severity_str(sev: Option<u64>) -> &'static str {
    match sev {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "info",
        _ => "hint",
    }
}

pub fn uri_to_path(uri: &str) -> PathBuf {
    let stripped = uri.strip_prefix("file:///").unwrap_or(uri);
    let decoded = stripped.replace("%3A", ":").replace("%20", " ");
    if decoded.len() > 1 && decoded.as_bytes().get(1) == Some(&b':') {
        PathBuf::from(decoded)
    } else {
        PathBuf::from(format!("/{decoded}"))
    }
}

pub fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

impl LspClient {
    pub async fn start(command: &str, args: &[String], root: &Path) -> Result<Self, LspError> {
        let mut child = Command::new(command)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;

        let stdin = Arc::new(Mutex::new(child.stdin.take().ok_or(LspError::Closed)?));
        let stdout = child.stdout.take().ok_or(LspError::Closed)?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let diags: DiagStore = Arc::new(Mutex::new(HashMap::new()));

        {
            let pending = pending.clone();
            let diags = diags.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                loop {
                    let msg = match read_frame(&mut reader).await {
                        Ok(m) => m,
                        Err(_) => break,
                    };
                    if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
                        if msg.get("method").is_none() {
                            if let Some(tx) = pending.lock().await.remove(&id) {
                                let _ = tx.send(msg);
                            }
                            continue;
                        }
                        continue;
                    }
                    if msg.get("method").and_then(|m| m.as_str())
                        == Some("textDocument/publishDiagnostics")
                    {
                        let params = &msg["params"];
                        let file = uri_to_path(params["uri"].as_str().unwrap_or(""));
                        let list: Vec<Diag> = params["diagnostics"]
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .map(|d| Diag {
                                        file: file.clone(),
                                        line: d["range"]["start"]["line"].as_u64().unwrap_or(0)
                                            as u32
                                            + 1,
                                        severity: severity_str(d["severity"].as_u64()).to_string(),
                                        message: d["message"].as_str().unwrap_or("").to_string(),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        diags.lock().await.insert(file, list);
                    }
                }
            });
        }

        let client = Self {
            child,
            stdin,
            pending,
            diags,
            next_id: AtomicI64::new(1),
            root: root.to_path_buf(),
        };

        let _init = client.request("initialize", json!({
            "processId": std::process::id(),
            "rootUri": path_to_uri(root),
            "capabilities": { "textDocument": { "publishDiagnostics": { "relatedInformation": false } } }
        }), Duration::from_secs(30)).await?;
        client.notify("initialized", json!({})).await?;
        Ok(client)
    }

    fn id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    async fn send(&self, msg: &Value) -> Result<(), LspError> {
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&encode_frame(msg)).await?;
        stdin.flush().await?;
        Ok(())
    }

    pub async fn request(
        &self,
        method: &str,
        params: Value,
        dur: Duration,
    ) -> Result<Value, LspError> {
        let id = self.id();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        timeout(dur, rx)
            .await
            .map_err(|_| LspError::Timeout(method.to_string()))?
            .map_err(|_| LspError::Closed)
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        self.send(&json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await
    }

    pub async fn open_document(
        &self,
        path: &Path,
        text: &str,
        version: i32,
    ) -> Result<(), LspError> {
        let lang = match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => "rust",
            Some("py") => "python",
            Some("ts") | Some("tsx") => "typescript",
            Some("js") | Some("jsx") => "javascript",
            Some("go") => "go",
            _ => "plaintext",
        };
        self.notify("textDocument/didOpen", json!({
            "textDocument": { "uri": path_to_uri(path), "languageId": lang, "version": version, "text": text }
        })).await
    }

    pub async fn diagnostics_for(&self, path: &Path, dur: Duration) -> Vec<Diag> {
        let deadline = tokio::time::Instant::now() + dur;
        loop {
            if let Some(d) = self.diags.lock().await.get(path) {
                return d.clone();
            }
            if tokio::time::Instant::now() >= deadline {
                return vec![];
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn all_diagnostics(&self) -> Vec<Diag> {
        let map = self.diags.lock().await;
        let mut all: Vec<Diag> = map.values().flatten().cloned().collect();
        all.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
        all
    }

    pub async fn shutdown(mut self) {
        let _ = self
            .request("shutdown", Value::Null, Duration::from_secs(3))
            .await;
        let _ = self.notify("exit", json!({})).await;
        let _ = timeout(Duration::from_secs(2), self.child.wait()).await;
        let _ = self.child.kill().await;
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip_encode() {
        let msg = json!({"jsonrpc":"2.0","id":1,"method":"x"});
        let bytes = encode_frame(&msg);
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("Content-Length: "));
        assert!(s.contains("\r\n\r\n{"));
    }

    #[tokio::test]
    async fn frame_roundtrip_decode() {
        let msg = json!({"jsonrpc":"2.0","id":7,"result":{"ok":true}});
        let bytes = encode_frame(&msg);
        let mut reader = BufReader::new(std::io::Cursor::new(bytes));
        let parsed = read_frame(&mut reader).await.unwrap();
        assert_eq!(parsed["id"], 7);
        assert_eq!(parsed["result"]["ok"], true);
    }

    #[tokio::test]
    async fn read_frame_rejects_garbage() {
        let mut reader = BufReader::new(std::io::Cursor::new(b"no headers here".to_vec()));
        assert!(read_frame(&mut reader).await.is_err());
    }

    #[test]
    fn uri_path_conversion_unix() {
        let p = uri_to_path("file:///home/user/a.rs");
        assert_eq!(p, PathBuf::from("/home/user/a.rs"));
    }

    #[test]
    fn uri_path_conversion_windows() {
        let p = uri_to_path("file:///C:/dev/a.rs");
        assert_eq!(p, PathBuf::from("C:/dev/a.rs"));
    }
}
