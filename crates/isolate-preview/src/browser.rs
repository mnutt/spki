//! The upstream browser SDK and a bounded WebSocket transport for standard two-party RPC.
use anyhow::{Result, ensure};
use futures::{SinkExt, StreamExt};
use isolate_protocol::{
    isolate_bridge_capnp::browser_isolate_bridge, web_session_capnp::web_session as web,
};
use std::{rc::Rc, sync::OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        protocol::{Role, WebSocketConfig},
    },
};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

pub fn module(path: &str) -> Option<&'static str> {
    if path == "__sandstorm/native-capnp/client.js" {
        return Some(include_str!("../browser/client.js"));
    }
    let name = if path.starts_with("capnp-es/") {
        path
    } else {
        let name = path.strip_prefix("__sandstorm/capnp/")?;
        name.strip_suffix(".js").unwrap_or(name)
    };
    static MODULES: OnceLock<std::collections::BTreeMap<String, String>> = OnceLock::new();
    MODULES
        .get_or_init(|| {
            let modules: std::collections::BTreeMap<String, String> =
                serde_json::from_str(include_str!("../sdk/modules.json"))
                    .expect("vendored SDK modules");
            modules
                .into_iter()
                .filter(|(name, _)| name.starts_with("capnp-es/") || name.starts_with("sandstorm/"))
                .map(|(name, source)| {
                    (
                        name,
                        source
                            .replace("\"capnp:/sandstorm/", "\"/__sandstorm/capnp/sandstorm/")
                            .replace("\"/sandstorm/", "\"/__sandstorm/capnp/sandstorm/"),
                    )
                })
                .collect()
        })
        .get(name)
        .map(String::as_str)
}
struct Input {
    writer: tokio::sync::Mutex<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Input {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl web::web_socket_stream::Server for Input {
    async fn send_bytes(
        self: Rc<Self>,
        params: web::web_socket_stream::SendBytesParams,
    ) -> capnp::Result<()> {
        let bytes = params.get()?.get_message()?;
        if bytes.len() > 1024 * 1024 {
            return Err(capnp::Error::failed("WebSocket chunk too large".into()));
        }
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            self.writer.lock().await.write_all(bytes).await
        })
        .await
        .map_err(|_| capnp::Error::failed("WebSocket input timed out".into()))?
        .map_err(|e| capnp::Error::failed(e.to_string()))
    }
}
pub fn connect(
    client: web::web_socket_stream::Client,
    runtime: &Rc<crate::Runtime>,
    headers: Vec<(String, String)>,
    session: &Rc<crate::bridge::Session>,
) -> capnp::Result<web::web_socket_stream::Client> {
    let worker = runtime.bridge.clone();
    let session_slot = session
        .socket_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| capnp::Error::overloaded("too many browser connections".into()))?;
    let worker_slot = worker
        .socket_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| capnp::Error::overloaded("too many worker connections".into()))?;
    let mut session_closed = session.closed.subscribe();
    let (raw, socket) = tokio::io::duplex(64 * 1024);
    let (mut output, input) = tokio::io::split(raw);
    let mut stopped = worker.stopped.subscribe();
    let bootstrap: browser_isolate_bridge::Client = capnp_rpc::new_client(crate::bridge::Browser {
        application: Some(crate::bridge::Application {
            runtime: Rc::downgrade(runtime),
            headers,
            capability: tokio::sync::OnceCell::new(),
        }),
        worker,
        session: Rc::downgrade(session),
    });
    let task = tokio::task::spawn_local(async move {
        let _slots = (session_slot, worker_slot);
        let result: Result<()> = async {
            let config = WebSocketConfig::default()
                .max_message_size(Some(1024 * 1024))
                .max_frame_size(Some(1024 * 1024))
                .max_write_buffer_size(2 * 1024 * 1024);
            let ws = WebSocketStream::from_raw_socket(socket, Role::Server, Some(config)).await;
            let (mut ws_out, mut ws_in) = ws.split();
            let (rpc, proxy) = tokio::io::duplex(64 * 1024);
            let (rpc_read, rpc_write) = tokio::io::split(rpc);
            let (proxy_read, mut proxy_write) = tokio::io::split(proxy);
            let network = capnp_rpc::twoparty::VatNetwork::new(
                rpc_read.compat(),
                rpc_write.compat_write(),
                capnp_rpc::rpc_twoparty_capnp::Side::Server,
                capnp::message::ReaderOptions {
                    traversal_limit_in_words: Some(128 * 1024),
                    nesting_limit: 64,
                },
            );
            let rpc = capnp_rpc::RpcSystem::new(Box::new(network), Some(bootstrap.client));
            let inbound = async {
                while let Some(message) = ws_in.next().await {
                    match message? {
                        Message::Binary(bytes) => {
                            proxy_write.write_all(&bytes).await?;
                        }
                        Message::Close(_) => break,
                        Message::Ping(_) | Message::Pong(_) => (),
                        _ => anyhow::bail!("RPC WebSocket requires binary messages"),
                    }
                }
                Ok::<_, anyhow::Error>(())
            };
            let outbound = async {
                let mut reader = proxy_read.compat();
                while let Some(message) = capnp_futures::serialize::try_read_message(
                    &mut reader,
                    capnp::message::ReaderOptions {
                        traversal_limit_in_words: Some(128 * 1024),
                        nesting_limit: 64,
                    },
                )
                .await?
                {
                    let mut bytes = Vec::new();
                    capnp::serialize::write_message_segments(&mut bytes, &message.into_segments())?;
                    ensure!(bytes.len() <= 1024 * 1024, "RPC message exceeds limit");
                    ws_out.send(Message::Binary(bytes.into())).await?;
                }
                Ok::<_, anyhow::Error>(())
            };
            let forward = async {
                let mut bytes = [0; 16384];
                loop {
                    let count = output.read(&mut bytes).await?;
                    if count == 0 {
                        break;
                    }
                    let mut request = client.send_bytes_request();
                    request.get().set_message(&bytes[..count]);
                    tokio::time::timeout(std::time::Duration::from_secs(20), request.send())
                        .await??;
                }
                Ok::<_, anyhow::Error>(())
            };
            tokio::select! {
                result = rpc => { result?; }
                result = inbound => { result?; }
                result = outbound => { result?; }
                result = forward => { result?; }
                _ = stopped.changed() => (),
                _ = session_closed.changed() => (),
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            eprintln!("browser RPC connection closed: {error}");
        }
    });
    Ok(capnp_rpc::new_client(Input {
        writer: tokio::sync::Mutex::new(input),
        task,
    }))
}

pub fn descriptor(path: &str) -> Option<Result<Vec<u8>>> {
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let kind = route.strip_prefix("__sandstorm/powerbox/")?;
    if !matches!(
        kind,
        "app-interface-descriptor" | "api-session-descriptor" | "outbound-http-descriptor"
    ) {
        return None;
    }
    Some((|| {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        use capnp::traits::HasTypeId;
        use isolate_protocol::{
            api_session_capnp::api_session,
            outbound_http_session_capnp::outbound_http_session as outbound,
            powerbox_capnp::powerbox_descriptor,
        };
        ensure!(query.len() <= 8192, "descriptor query too long");
        let mut fields = std::collections::BTreeMap::<String, Vec<String>>::new();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            let decode = |value: &str| -> Result<String> {
                Ok(
                    percent_encoding::percent_decode_str(&value.replace('+', " "))
                        .decode_utf8()?
                        .into_owned(),
                )
            };
            fields
                .entry(decode(name)?)
                .or_default()
                .push(decode(value)?);
        }
        let single = |name: &str| -> Result<&str> {
            let values = fields.get(name);
            ensure!(values.is_none_or(|v| v.len() == 1), "duplicate {name}");
            Ok(values.map_or("", |v| v[0].as_str()))
        };
        let mut message = capnp::message::Builder::new_default();
        let mut tag = message
            .init_root::<powerbox_descriptor::Builder>()
            .init_tags(1)
            .get(0);
        let id = match kind {
            "app-interface-descriptor" => {
                let text = single("interfaceId")?;
                let id = if let Some(hex) = text.strip_prefix("0x") {
                    u64::from_str_radix(hex, 16)?
                } else {
                    text.parse()?
                };
                ensure!(id != 0, "interface ID must not be zero");
                ensure!(
                    single("interfaceName")?.len() <= 512,
                    "interface name too long"
                );
                id
            }
            "api-session-descriptor" => {
                let mut value = tag
                    .reborrow()
                    .get_value()
                    .init_as::<api_session::powerbox_tag::Builder>();
                value.set_canonical_url(single("canonicalUrl")?);
                if let Some(scopes) = fields.get("oauthScope") {
                    ensure!(scopes.len() <= 64, "too many OAuth scopes");
                    let mut list = value.init_oauth_scopes(scopes.len() as u32);
                    for (i, scope) in scopes.iter().enumerate() {
                        list.set(i as u32, scope);
                    }
                }
                api_session::Client::TYPE_ID
            }
            _ => {
                let url = single("baseUrl")?;
                ensure!(
                    url.starts_with("https://") || url.starts_with("http://"),
                    "baseUrl must be HTTP(S)"
                );
                let mut value = tag
                    .reborrow()
                    .get_value()
                    .init_as::<outbound::powerbox_tag::Builder>();
                value.set_base_url(url);
                if let Some(methods) = fields.get("method") {
                    ensure!(methods.len() <= 7, "too many methods");
                    let mut list = value.init_methods(methods.len() as u32);
                    for (i, method) in methods.iter().enumerate() {
                        let method = match method.as_str() {
                            "GET" => outbound::Method::Get,
                            "POST" => outbound::Method::Post,
                            "PUT" => outbound::Method::Put,
                            "PATCH" => outbound::Method::Patch,
                            "DELETE" => outbound::Method::Delete,
                            "HEAD" => outbound::Method::Head,
                            "OPTIONS" => outbound::Method::Options,
                            _ => anyhow::bail!("invalid HTTP method"),
                        };
                        list.set(i as u32, method);
                    }
                }
                outbound::Client::TYPE_ID
            }
        };
        tag.set_id(id);
        let mut bytes = Vec::new();
        capnp::serialize_packed::write_message(&mut bytes, &message)?;
        Ok(serde_json::to_vec(
            &serde_json::json!({"ok":true,"descriptor":URL_SAFE_NO_PAD.encode(bytes),"interfaceId":id.to_string(),"interfaceName":single("interfaceName")?}),
        )?)
    })())
}
