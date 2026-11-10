//! A separate streaming connection keeps app logs independent of serial builds/deployments.
use crate::credentials::{Credential, sandstorm_loopback};
use anyhow::{Context, Result, ensure};
use spki_common::protocol::Entry;
use std::{sync::mpsc, time::Duration};

pub(crate) struct Follower {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Follower {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Follower {
    pub fn start(credential: Credential) -> Result<Self> {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let (ready, connected) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new().name("spki-logs".into()).spawn(move || {
            let run = || -> Result<()> {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                runtime.block_on(async {
                    tokio::select! {
                        _ = stopped => (),
                        result = follow(credential, &ready) => { if let Err(error) = result { eprintln!("[logs] {error}"); } }
                    }
                });
                Ok(())
            };
            if run().is_err() { let _ = ready.send(Err("could not start log reader")); }
        })?;
        let follower = Self {
            stop: Some(stop),
            thread: Some(thread),
        };
        connected
            .recv_timeout(Duration::from_secs(15))
            .context("log stream did not connect")?
            .map_err(anyhow::Error::msg)?;
        Ok(follower)
    }
}
fn client(credential: &Credential) -> Result<reqwest::Client> {
    let url = reqwest::Url::parse(&credential.endpoint)?;
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(25)); // Idle heartbeats arrive every ten seconds.
    if url.scheme() == "http" && credential.allow_local_http {
        builder = builder.no_proxy();
        if let Some(host) = url.host_str().filter(|host| sandstorm_loopback(host)) {
            builder = builder.resolve(
                host,
                std::net::SocketAddr::from(([127, 0, 0, 1], url.port_or_known_default().unwrap())),
            );
        }
    }
    Ok(builder.build()?)
}
async fn follow(
    credential: Credential,
    ready: &mpsc::SyncSender<std::result::Result<(), &'static str>>,
) -> Result<()> {
    let client = client(&credential)?;
    let mut cursor: Option<String> = None;
    let mut connected_once = false;
    let mut retry_seconds = 1;
    let mut terminal_error = None;
    loop {
        let mut url = reqwest::Url::parse(&format!("{}__spki/v1/logs", credential.endpoint))?;
        if let Some(id) = &cursor {
            url.query_pairs_mut().append_pair("after", id);
        }
        let result: Result<()> = async {
            let mut response = client
                .get(url)
                .bearer_auth(&credential.token)
                .header("Accept", "text/event-stream")
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("log connection interrupted"))?;
            if !response.status().is_success() {
                let message = if matches!(response.status().as_u16(), 401 | 403) {
                    "log access denied; check or renew your Developer webkey"
                } else {
                    "Preview log endpoint unavailable; update the Preview app"
                };
                if matches!(response.status().as_u16(), 401 | 403 | 404 | 410) {
                    terminal_error = Some(message);
                }
                anyhow::bail!(message);
            }
            ensure!(
                response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.split(';').next() == Some("text/event-stream")),
                "invalid log stream response"
            );
            if !connected_once {
                let _ = ready.send(Ok(()));
                connected_once = true;
            }
            let mut decoder = Decoder::default();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| anyhow::anyhow!("log connection interrupted"))?
            {
                for event in decoder.feed(&chunk)? {
                    if let Some((id, message)) =
                        event_message(event, cursor.as_deref(), &credential.token)?
                    {
                        for line in message.lines() {
                            eprintln!("[app] {line}");
                        }
                        cursor = Some(id);
                    }
                }
                retry_seconds = 1;
            }
            anyhow::bail!("log stream closed")
        }
        .await;
        if let Some(message) = terminal_error {
            if !connected_once {
                let _ = ready.send(Err(message));
            } else {
                eprintln!("[logs] {message}");
            }
            return Ok(());
        }
        if !connected_once {
            let _ = ready.send(Err("could not connect to Preview logs"));
            return Ok(());
        }
        if result.is_err() && retry_seconds == 1 {
            eprintln!("[logs] Connection interrupted; reconnecting…");
        }
        tokio::time::sleep(Duration::from_secs(retry_seconds)).await;
        retry_seconds = (retry_seconds * 2).min(10);
    }
}
#[derive(Default)]
struct Decoder {
    bytes: Vec<u8>,
}
struct Event {
    id: Option<String>,
    kind: String,
    data: String,
}
impl Decoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        for &byte in bytes {
            self.bytes.push(byte);
            ensure!(self.bytes.len() <= 65536, "oversized log event");
            if self.bytes.ends_with(b"\n\n") || self.bytes.ends_with(b"\r\n\r\n") {
                let text = std::str::from_utf8(&self.bytes).context("invalid log encoding")?;
                let mut event = Event {
                    id: None,
                    kind: String::new(),
                    data: String::new(),
                };
                for line in text.lines() {
                    if let Some(value) = line.strip_prefix("id:") {
                        event.id = Some(value.trim().into());
                    }
                    if let Some(value) = line.strip_prefix("event:") {
                        event.kind = value.trim().into();
                    }
                    if let Some(value) = line.strip_prefix("data:") {
                        event.data.push_str(value.trim_start());
                        event.data.push('\n');
                    }
                }
                if event.id.is_some() {
                    events.push(event);
                }
                self.bytes.clear();
            }
        }
        Ok(events)
    }
}
fn cursor_parts(id: &str) -> Result<(&str, u64)> {
    let (epoch, sequence) = id.split_once(':').context("invalid log event ID")?;
    ensure!(
        epoch.len() == 32
            && epoch.bytes().all(|b| b.is_ascii_hexdigit())
            && sequence.len() <= 20
            && !sequence.is_empty()
            && sequence.bytes().all(|b| b.is_ascii_digit()),
        "invalid log event ID"
    );
    Ok((epoch, sequence.parse()?))
}
fn event_message(
    event: Event,
    previous: Option<&str>,
    secret: &str,
) -> Result<Option<(String, String)>> {
    let id = event.id.context("missing log event ID")?;
    let (epoch, sequence) = cursor_parts(&id)?;
    if let Some(previous) = previous {
        let (old_epoch, old_sequence) = cursor_parts(previous)?;
        if epoch == old_epoch && sequence <= old_sequence {
            return Ok(None);
        }
    }
    let message = match event.kind.as_str() {
        "log" => {
            serde_json::from_str::<Entry>(&event.data)
                .context("invalid log event")?
                .message
        }
        "gap" => "[logs] Older entries were dropped from the grain's log buffer.".into(),
        "reset" => "[logs] Grain restarted; resuming its log stream.".into(),
        _ => anyhow::bail!("unknown log event"),
    };
    Ok(Some((id, sanitize(&message.replace(secret, "[redacted]")))))
}
fn sanitize(text: &str) -> String {
    let mut safe = String::new();
    for c in text.chars() {
        if (c.is_control() && c != '\n' && c != '\t')
            || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            safe.extend(c.escape_default());
        } else {
            safe.push(c);
        }
    }
    safe
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_fragmented_events_redacts_secrets_and_escapes_terminal_controls() {
        let id = format!("{}:1", "a".repeat(32));
        let entry = Entry {
            timestamp_ms: 0,
            source: "stderr".into(),
            message: "hello secret\u{1b}[2J\r\u{202e}world".into(),
        };
        let frame = format!(
            ": heartbeat\n\nid: {id}\nevent: log\ndata: {}\n\n",
            serde_json::to_string(&entry).unwrap()
        );
        let mut decoder = Decoder::default();
        let mut messages = Vec::new();
        for byte in frame.bytes() {
            messages.extend(decoder.feed(&[byte]).unwrap());
        }
        assert_eq!(messages.len(), 1);
        let (cursor, message) = event_message(messages.pop().unwrap(), None, "secret")
            .unwrap()
            .unwrap();
        assert_eq!(cursor, id);
        assert!(message.contains("[redacted]"));
        assert!(!message.contains(['\u{1b}', '\r', '\u{202e}']));
        let event = decoder.feed(frame.as_bytes()).unwrap().pop().unwrap();
        assert!(event_message(event, Some(&id), "secret").unwrap().is_none());
        assert!(Decoder::default().feed(&vec![b'x'; 65537]).is_err());
    }
}
