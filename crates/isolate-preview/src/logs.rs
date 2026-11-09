//! Bounded app log capture and push streaming through WebSession's ByteStream.
use anyhow::{Result, ensure};
use isolate_protocol::{
    util_capnp::{byte_stream, handle},
    web_session_capnp::web_session as web,
};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
const MAX_BYTES: usize = 256 * 1024;
const MAX_ENTRIES: usize = 2048;
const MAX_LINE: usize = 8192;
const MAX_SUBSCRIBERS: usize = 16;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub timestamp_ms: u64,
    pub source: String,
    pub message: String,
}
struct Stored {
    sequence: u64,
    entry: Entry,
    bytes: usize,
}
struct Buffer {
    entries: VecDeque<Stored>,
    bytes: usize,
    sequence: u64,
}
pub struct Logs {
    epoch: String,
    buffer: RefCell<Buffer>,
    changed: tokio::sync::watch::Sender<u64>,
    subscribers: Cell<usize>,
}
#[derive(Clone, Debug)]
struct Cursor {
    epoch: String,
    sequence: u64,
}
impl Cursor {
    fn parse(value: &str) -> Result<Self> {
        let (epoch, sequence) = value
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("invalid log cursor"))?;
        ensure!(
            epoch.len() == 32 && epoch.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid log cursor"
        );
        ensure!(
            !sequence.is_empty()
                && sequence.len() <= 20
                && sequence.bytes().all(|b| b.is_ascii_digit()),
            "invalid log cursor"
        );
        Ok(Self {
            epoch: epoch.into(),
            sequence: sequence.parse()?,
        })
    }
}
impl Logs {
    pub fn new() -> Result<Rc<Self>> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes)
            .map_err(|_| anyhow::anyhow!("could not create log stream identity"))?;
        let (changed, _) = tokio::sync::watch::channel(0);
        Ok(Rc::new(Self {
            epoch: bytes.iter().map(|b| format!("{b:02x}")).collect(),
            buffer: RefCell::new(Buffer {
                entries: VecDeque::new(),
                bytes: 0,
                sequence: 0,
            }),
            changed,
            subscribers: Cell::new(0),
        }))
    }
    pub fn push(&self, source: &str, message: &str) {
        let mut end = message.len().min(MAX_LINE);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        let message = message[..end].to_owned();
        let bytes = message.len() + source.len() + 64;
        let mut buffer = self.buffer.borrow_mut();
        buffer.sequence += 1;
        let sequence = buffer.sequence;
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        buffer.entries.push_back(Stored {
            sequence,
            entry: Entry {
                timestamp_ms,
                source: source.into(),
                message,
            },
            bytes,
        });
        buffer.bytes += bytes;
        while buffer.bytes > MAX_BYTES || buffer.entries.len() > MAX_ENTRIES {
            if let Some(old) = buffer.entries.pop_front() {
                buffer.bytes -= old.bytes;
            }
        }
        self.changed.send_replace(sequence);
    }
    fn batch(&self, cursor: &mut Cursor) -> Vec<u8> {
        let buffer = self.buffer.borrow();
        let oldest = buffer
            .entries
            .front()
            .map_or(buffer.sequence + 1, |entry| entry.sequence);
        let mut output = String::new();
        if cursor.epoch != self.epoch || cursor.sequence > buffer.sequence {
            cursor.epoch = self.epoch.clone();
            cursor.sequence = oldest - 1;
            output.push_str(&format!(
                "id: {}:{}\nevent: reset\ndata: {{\"message\":\"grain log stream restarted\"}}\n\n",
                self.epoch, cursor.sequence
            ));
        } else if cursor.sequence < oldest - 1 {
            cursor.sequence = oldest - 1;
            output.push_str(&format!("id: {}:{}\nevent: gap\ndata: {{\"message\":\"older log entries were dropped\"}}\n\n", self.epoch, cursor.sequence));
        }
        for stored in &buffer.entries {
            if stored.sequence <= cursor.sequence {
                continue;
            }
            let data = serde_json::to_string(&stored.entry).expect("log entry serializes");
            output.push_str(&format!(
                "id: {}:{}\nevent: log\ndata: {data}\n\n",
                self.epoch, stored.sequence
            ));
            cursor.sequence = stored.sequence;
            if output.len() >= 16384 {
                break;
            }
        }
        output.into_bytes()
    }
    pub fn serve(
        self: &Rc<Self>,
        stream: byte_stream::Client,
        query: Option<&str>,
        mut response: web::response::Builder<'_>,
    ) -> Result<()> {
        let mut cursor = if let Some(query) = query {
            ensure!(query.len() <= 128, "invalid log cursor");
            let value = query
                .strip_prefix("after=")
                .ok_or_else(|| anyhow::anyhow!("expected after log cursor"))?;
            Cursor::parse(&percent_encoding::percent_decode_str(value).decode_utf8()?)?
        } else {
            Cursor {
                epoch: self.epoch.clone(),
                sequence: 0,
            }
        };
        ensure!(
            self.subscribers.get() < MAX_SUBSCRIBERS,
            "too many log subscribers"
        );
        self.subscribers.set(self.subscribers.get() + 1);
        let logs = self.clone();
        let mut changed = self.changed.subscribe();
        let task = tokio::task::spawn_local(async move {
            async fn write(stream: &byte_stream::Client, bytes: &[u8]) -> Result<()> {
                let mut request = stream.write_request();
                request.get().set_data(bytes);
                // Backpressure is local to this subscriber, never to log capture or a worker.
                tokio::time::timeout(Duration::from_secs(20), request.send()).await??;
                Ok(())
            }
            let result: Result<()> = async {
                write(&stream, b": connected\n\n").await?;
                loop {
                    let bytes = logs.batch(&mut cursor);
                    if !bytes.is_empty() { write(&stream, &bytes).await?; continue; }
                    tokio::select! {
                        result = changed.changed() => { result?; }
                        _ = tokio::time::sleep(Duration::from_secs(10)) => { write(&stream, b": heartbeat\n\n").await?; }
                    }
                }
            }.await;
            // Dropping the byte stream closes an interrupted HTTP response. Reconnect uses IDs.
            let _ = result;
        });
        let handle: handle::Client = capnp_rpc::new_client(Subscription {
            task,
            logs: self.clone(),
        });
        response.reborrow().init_cache_policy();
        let mut content = response.init_content();
        content.set_mime_type("text/event-stream; charset=utf-8");
        content.init_body().set_stream(handle);
        Ok(())
    }
}
struct Subscription {
    task: tokio::task::JoinHandle<()>,
    logs: Rc<Logs>,
}
impl handle::Server for Subscription {
    async fn ping(
        self: Rc<Self>,
        _: handle::PingParams,
        _: handle::PingResults,
    ) -> capnp::Result<()> {
        if self.task.is_finished() {
            Err(capnp::Error::disconnected("log stream closed".into()))
        } else {
            Ok(())
        }
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.task.abort();
        self.logs.subscribers.set(self.logs.subscribers.get() - 1);
    }
}
/// Read fixed-size chunks so a worker cannot allocate unbounded memory with a long line.
/// Preserve output in Sandstorm's ordinary log as well as our bounded capture buffer.
pub async fn capture(mut reader: impl AsyncRead + Unpin, source: &'static str, logs: Rc<Logs>) {
    let mut chunk = [0u8; 4096];
    let mut line = Vec::new();
    let mut stderr = tokio::io::stderr();
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &byte in &chunk[..n] {
            if byte == b'\n' {
                logs.push(source, &String::from_utf8_lossy(&line));
                line.clear();
            } else {
                line.push(byte);
                if line.len() >= MAX_LINE {
                    logs.push(source, &String::from_utf8_lossy(&line));
                    line.clear();
                }
            }
        }
        let _ = stderr.write_all(&chunk[..n]).await;
    }
    if !line.is_empty() {
        logs.push(source, &String::from_utf8_lossy(&line));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_is_bounded_and_reports_gaps_and_restarts() {
        let logs = Logs::new().unwrap();
        let mut cursor = Cursor {
            epoch: logs.epoch.clone(),
            sequence: 0,
        };
        logs.push("stderr", "one");
        let bytes = String::from_utf8(logs.batch(&mut cursor)).unwrap();
        assert!(bytes.contains("one"));
        assert!(logs.batch(&mut cursor).is_empty());
        for _ in 0..3000 {
            logs.push("stderr", &"é".repeat(6000));
        }
        assert!(logs.buffer.borrow().bytes <= MAX_BYTES);
        let bytes = String::from_utf8(logs.batch(&mut cursor)).unwrap();
        assert!(bytes.contains("event: gap"));
        let next = Logs::new().unwrap();
        assert!(
            String::from_utf8(next.batch(&mut cursor))
                .unwrap()
                .contains("event: reset")
        );
        assert!(Cursor::parse("wrong:1").is_err());
    }
    struct Slow;
    impl byte_stream::Server for Slow {
        async fn write(self: Rc<Self>, _: byte_stream::WriteParams) -> capnp::Result<()> {
            std::future::pending().await
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn slow_subscriber_does_not_block_capture_and_drop_releases_it() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let logs = Logs::new().unwrap();
                let stream: byte_stream::Client = capnp_rpc::new_client(Slow);
                let mut message = capnp::message::Builder::new_default();
                use capnp::traits::ImbueMut;
                let mut capabilities = Vec::new();
                let mut response = message.init_root::<web::response::Builder>();
                response.imbue_mut(&mut capabilities);
                logs.serve(stream, None, response).unwrap();
                tokio::task::yield_now().await;
                for _ in 0..3000 {
                    logs.push("stdout", "still running");
                }
                assert_eq!(logs.buffer.borrow().sequence, 3000);
                assert_eq!(logs.subscribers.get(), 1);
                drop(message);
                drop(capabilities);
                tokio::task::yield_now().await;
                assert_eq!(logs.subscribers.get(), 0);
            })
            .await;
    }
}
