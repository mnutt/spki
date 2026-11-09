use crate::{
    http,
    transport::{Body, ResponseState},
};
use capnp_rpc::new_client;
use isolate_protocol::isolate_host_capnp::isolate_binding_services;
use std::{
    cell::RefCell,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    rc::Rc,
};
const LIMIT: usize = 1024 * 1024;
// Listing is all-or-error, never a silently truncated list. These bounds also keep
// its serialized response below the 1 MiB transport limit even at maximum key size.
const MAX_LIST_KEYS: usize = 4096;
const MAX_DIRECTORY_ENTRIES: usize = 8192;
pub struct Bindings {
    pub(crate) root: PathBuf,
    pub bridge: Option<Rc<crate::bridge::Worker>>,
}
impl Bindings {
    pub fn new(root: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self { root, bridge: None })
    }
}
impl isolate_binding_services::Server for Bindings {
    async fn get_bridge(
        self: Rc<Self>,
        params: isolate_binding_services::GetBridgeParams,
        mut results: isolate_binding_services::GetBridgeResults,
    ) -> capnp::Result<()> {
        use capnp::capability::FromClientHook;
        if params.get()?.get_name()? != "sandstorm" {
            return Err(capnp::Error::unimplemented("unknown bridge".into()));
        }
        let bridge = self
            .bridge
            .as_ref()
            .ok_or_else(|| capnp::Error::failed("bridge unavailable".into()))?;
        let client: isolate_protocol::isolate_bridge_capnp::isolate_bridge::Client =
            capnp_rpc::new_client_from_rc(bridge.clone());
        results
            .get()
            .get_bridge()
            .set_as_capability(client.into_client_hook());
        Ok(())
    }
    async fn get_service(
        self: Rc<Self>,
        params: isolate_binding_services::GetServiceParams,
        mut results: isolate_binding_services::GetServiceResults,
    ) -> capnp::Result<()> {
        if params.get()?.get_name()? != "storage" {
            return Err(capnp::Error::unimplemented("unknown binding".into()));
        }
        results
            .get()
            .set_service(new_client(Storage(self.root.clone())));
        Ok(())
    }
}
struct Storage(PathBuf);
struct Complete;
impl http::http_service::server_request_context::Server for Complete {}
impl http::http_service::Server for Storage {
    async fn start_request(
        self: Rc<Self>,
        params: http::http_service::StartRequestParams,
        mut results: http::http_service::StartRequestResults,
    ) -> capnp::Result<()> {
        let params = params.get()?;
        let request = params.get_request()?;
        let method = request.get_method()?;
        let url = request.get_url()?.to_str()?.to_owned();
        let context = params.get_context()?;
        let (sender, receiver) = futures::channel::oneshot::channel();
        let state = Rc::new(RefCell::new(ResponseState {
            complete: Some(sender),
            ..Default::default()
        }));
        match request.get_body_size().which()? {
            http::http_request::body_size::Fixed(0) => {
                state
                    .borrow_mut()
                    .complete
                    .take()
                    .unwrap()
                    .send(Ok(()))
                    .ok();
            }
            http::http_request::body_size::Fixed(n) if n > LIMIT as u64 => {
                return Err(capnp::Error::failed("storage value exceeds 1 MiB".into()));
            }
            http::http_request::body_size::Fixed(n) => {
                state.borrow_mut().expected = Some(n as usize);
                results
                    .get()
                    .set_request_body(new_client(Body(state.clone())));
            }
            _ => results
                .get()
                .set_request_body(new_client(Body(state.clone()))),
        }
        let pending: http::http_service::server_request_context::Client =
            capnp_rpc::new_future_client(async move {
                receiver.await.map_err(error)?.map_err(error)?;
                let body = std::mem::take(&mut state.borrow_mut().body);
                let root = self.0.clone();
                let (status, mime, mut bytes) = crate::disk::run(move || {
                    Storage(root).handle(
                        if method == http::HttpMethod::Head {
                            http::HttpMethod::Get
                        } else {
                            method
                        },
                        &url,
                        &body,
                    )
                })
                .await
                .map_err(error)?;
                let head_bytes = if method == http::HttpMethod::Head && status == 200 {
                    Some(bytes.len())
                } else {
                    None
                };
                if method == http::HttpMethod::Head {
                    bytes.clear();
                }
                let mut response = context.start_response_request();
                let mut metadata = response.get().init_response();
                metadata.set_status_code(status);
                metadata
                    .reborrow()
                    .init_body_size()
                    .set_fixed(bytes.len() as u64);
                let mut headers = metadata.init_headers(if head_bytes.is_some() { 2 } else { 1 });
                if let Some(size) = head_bytes {
                    let mut header = headers.reborrow().get(1).init_uncommon();
                    header.set_name("X-Sandstorm-Storage-Bytes");
                    header.set_value(size.to_string());
                }
                let mut header = headers.get(0).init_uncommon();
                header.set_name("Content-Type");
                header.set_value(mime);
                let result = response.send().promise.await?;
                if !bytes.is_empty() {
                    let stream = result.get()?.get_body()?;
                    let mut write = stream.write_request();
                    write.get().set_bytes(&bytes);
                    write.send().await?;
                    stream.end_request().send().promise.await?;
                }
                Ok(new_client(Complete))
            });
        results.get().set_context(pending);
        Ok(())
    }
}
fn error(e: impl std::fmt::Display) -> capnp::Error {
    capnp::Error::failed(e.to_string())
}
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && !key.starts_with('.')
        && key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}
impl Storage {
    fn handle(
        &self,
        method: http::HttpMethod,
        url: &str,
        body: &[u8],
    ) -> anyhow::Result<(u16, &'static str, Vec<u8>)> {
        let path = url
            .split_once("://")
            .and_then(|(_, u)| u.split_once('/'))
            .map(|(_, p)| p)
            .unwrap_or("");
        let key = percent_encoding::percent_decode_str(path).decode_utf8()?;
        let json = |status, value: serde_json::Value| {
            Ok((status, "application/json", value.to_string().into_bytes()))
        };
        if key.is_empty() && method == http::HttpMethod::Get {
            let mut keys = Vec::new();
            let mut total = 0u64;
            for (scanned, entry) in std::fs::read_dir(&self.0)?.enumerate() {
                if scanned >= MAX_DIRECTORY_ENTRIES || keys.len() >= MAX_LIST_KEYS {
                    return json(
                        413,
                        serde_json::json!({"ok":false,"error":"storage listing exceeds 4096 keys or 8192 directory entries"}),
                    );
                }
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                if valid_key(&name) && entry.file_type()?.is_file() {
                    let bytes = entry.metadata()?.len();
                    total = total
                        .checked_add(bytes)
                        .ok_or_else(|| anyhow::anyhow!("storage size overflow"))?;
                    keys.push(serde_json::json!({"name":name,"bytes":bytes}));
                }
            }
            return json(
                200,
                serde_json::json!({"ok":true,"keys":keys,"totalBytes":total}),
            );
        }
        if !valid_key(&key) {
            return json(
                400,
                serde_json::json!({"ok":false,"error":"invalid storage key"}),
            );
        }
        let path = self.0.join(key.as_ref());
        match method {
            http::HttpMethod::Get => {
                match std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(path)
                {
                    Ok(file) => {
                        let mut bytes = Vec::new();
                        file.take((LIMIT + 1) as u64).read_to_end(&mut bytes)?;
                        anyhow::ensure!(bytes.len() <= LIMIT, "stored value exceeds limit");
                        Ok((200, "application/octet-stream", bytes))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        json(404, serde_json::json!({"ok":false,"error":"not found"}))
                    }
                    Err(e) => Err(e.into()),
                }
            }
            http::HttpMethod::Put => {
                anyhow::ensure!(body.len() <= LIMIT, "storage value too large");
                let mut file = tempfile::NamedTempFile::new_in(&self.0)?;
                file.write_all(body)?;
                file.as_file().sync_all()?;
                file.persist(path)?;
                std::fs::File::open(&self.0)?.sync_all()?;
                json(200, serde_json::json!({"ok":true,"bytes":body.len()}))
            }
            http::HttpMethod::Delete => {
                match std::fs::remove_file(path) {
                    Ok(()) => std::fs::File::open(&self.0)?.sync_all()?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                json(200, serde_json::json!({"ok":true}))
            }
            _ => json(
                405,
                serde_json::json!({"ok":false,"error":"unsupported storage method"}),
            ),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_reject_traversal() {
        for key in ["", ".", "..", "../x", "a/b", "/var/log", "a\\b"] {
            assert!(!valid_key(key));
        }
        assert!(valid_key("counter"));
    }
    #[test]
    fn value_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        Storage(dir.path().into())
            .handle(http::HttpMethod::Put, "http://storage/counter", b"7")
            .unwrap();
        assert_eq!(
            Storage(dir.path().into())
                .handle(http::HttpMethod::Get, "http://storage/counter", b"")
                .unwrap()
                .2,
            b"7"
        );
    }
    #[test]
    fn delete_is_idempotent_and_listing_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage(dir.path().into());
        storage
            .handle(http::HttpMethod::Put, "http://storage/key", b"value")
            .unwrap();
        for _ in 0..2 {
            assert_eq!(
                storage
                    .handle(http::HttpMethod::Delete, "http://storage/key", b"")
                    .unwrap()
                    .0,
                200
            );
        }
        assert_eq!(
            storage
                .handle(http::HttpMethod::Get, "http://storage/key", b"")
                .unwrap()
                .0,
            404
        );
        for i in 0..4097 {
            std::fs::write(dir.path().join(format!("key-{i}")), b"").unwrap();
        }
        assert_eq!(
            storage
                .handle(http::HttpMethod::Get, "http://storage/", b"")
                .unwrap()
                .0,
            413
        );
    }
}
