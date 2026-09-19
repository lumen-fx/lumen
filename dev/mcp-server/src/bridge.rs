//! TCP bridge: maintains a single connection to the Lumen app and ferries
//! JSON-RPC requests/responses over it.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::Mutex;

/// A pooled, serialized TCP connection to the in-app JSON-RPC server. The
/// MCP server is single-threaded enough that one connection + a mutex is
/// fine.
pub struct LumenBridge {
    host: String,
    port: u16,
    next_id: AtomicU64,
    conn: Mutex<Option<Conn>>,
}

struct Conn {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl LumenBridge {
    /// Build a bridge that lazily connects to `host:port`.
    pub fn new(host: String, port: u16) -> Self {
        Self {
            host,
            port,
            next_id: AtomicU64::new(1),
            conn: Mutex::new(None),
        }
    }

    async fn connect(&self) -> std::io::Result<Conn> {
        let stream = TcpStream::connect((self.host.as_str(), self.port)).await?;
        let (r, w) = stream.into_split();
        Ok(Conn {
            reader: BufReader::new(r),
            writer: w,
        })
    }

    /// Send a JSON-RPC call and await its response. Returns the JSON value of
    /// `result`, or an `Err(String)` with the JSON-RPC error message / IO
    /// error.
    pub async fn call(&self, method: &str, params: Option<Value>) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params.unwrap_or(Value::Null),
        });
        let mut line = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
        line.push(b'\n');

        let mut guard = self.conn.lock().await;
        // (Re)connect on first call or after a transport failure.
        if guard.is_none() {
            *guard = Some(self.connect().await.map_err(|e| {
                format!(
                    "lumen app not running on {}:{port} - start your Lumen example with \
                     LumenMcpPlugin installed (io error: {e})",
                    self.host,
                    port = self.port
                )
            })?);
        }

        // Send + receive. On IO failure, drop conn so next call reconnects.
        let attempt: Result<Value, String> = async {
            let conn = guard.as_mut().expect("conn present");
            conn.writer
                .write_all(&line)
                .await
                .map_err(|e| format!("write failed: {e}"))?;
            let mut resp = String::new();
            let n = conn
                .reader
                .read_line(&mut resp)
                .await
                .map_err(|e| format!("read failed: {e}"))?;
            if n == 0 {
                return Err("lumen app closed the connection (EOF)".into());
            }
            let v: Value = serde_json::from_str(resp.trim())
                .map_err(|e| format!("invalid JSON from app: {e}"))?;
            if let Some(err) = v.get("error") {
                let msg = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown JSON-RPC error");
                return Err(format!("lumen JSON-RPC error: {msg}"));
            }
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        }
        .await;
        if attempt.is_err() {
            *guard = None;
        }
        attempt
    }
}
