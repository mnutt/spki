#![forbid(unsafe_code)]
pub mod bindings;
pub mod bridge;
pub mod browser;
pub mod controller;
mod disk;
pub mod grain;
mod http_adapter;
pub mod logs;
pub mod protocol;
mod reload;
mod routes;
pub mod storage;
use anyhow::{Result, ensure};
use futures::AsyncReadExt;
use isolate_protocol::{
    http_over_capnp_capnp as http, isolate_host_capnp::isolate_host, isolate_worker_source_capnp,
};
use std::{cell::RefCell, rc::Rc};
use tokio_util::compat::TokioAsyncReadCompatExt;

pub const HOST_SHA256: &str = "94b04f0ed5ad0f3ff862dcadd1b7edfe6cad1717587a60b7d8e6775541b538b1";
pub mod transport;
use transport::{HttpResponse, ResponseContext, ResponseState};
pub fn bundle(js: &[u8]) -> Result<Vec<u8>> {
    ensure!(js.len() <= 8 * 1024 * 1024, "worker exceeds 8 MiB");
    let text = std::str::from_utf8(js)?;
    let allocator = oxc_allocator::Allocator::default();
    let parsed = oxc_parser::Parser::new(
        &allocator,
        text,
        oxc_span::SourceType::from_path("application.mjs")?,
    )
    .parse();
    if let Some(error) = parsed.diagnostics.into_iter().next() {
        anyhow::bail!(
            "JavaScript syntax error in application.js: {}",
            error
                .render_with_source_code(text)
                .chars()
                .take(8192)
                .collect::<String>()
        );
    }

    let mut message = capnp::message::Builder::new_default();
    let mut source =
        message.init_root::<isolate_worker_source_capnp::isolate_worker_source::Builder>();
    source.set_format_version(1);
    source.set_main_module("main.js");
    source.set_compatibility_date("2026-06-10");
    // Request-local channel ownership, including calls through cached SDK objects.
    source
        .reborrow()
        .init_compatibility_flags(1)
        .set(0, "nodejs_als");
    let custom = bindings::read(text)?;
    let mut bindings = source.reborrow().init_bindings((custom.len() + 2) as u32);
    for (index, value) in custom.iter().enumerate() {
        let mut binding = bindings.reborrow().get(index as u32 + 2);
        binding.set_name(&value.name);
        match &value.value {
            bindings::Value::Text(_) => binding.set_text(&value.bytes()?),
            bindings::Value::Json(_) => binding.set_json(&value.bytes()?),
            bindings::Value::Data(_) => binding.set_data(&value.bytes()?),
            bindings::Value::Service(target) => binding.set_service(target),
        }
    }
    let mut binding = bindings.reborrow().get(1);
    binding.set_name("__SANDSTORM_NATIVE_CAPNP");
    binding.set_capnp("sandstorm");
    let mut binding = bindings.get(0);
    binding.set_name("STORAGE");
    binding.set_http_service("storage");
    let sdk: std::collections::BTreeMap<String, String> =
        serde_json::from_str(include_str!("../sdk/modules.json"))?;
    let wrapper = include_bytes!("../sdk/entry.js");
    let mut modules = source.init_modules((sdk.len() + 2) as u32);
    let mut entry = modules.reborrow().get(0);
    entry.set_name("main.js");
    entry.set_es_module(wrapper);
    let mut entry = modules.reborrow().get(1);
    entry.set_name("application.js");
    entry.set_es_module(js);
    for (i, (name, body)) in sdk.iter().enumerate() {
        let mut module = modules.reborrow().get(i as u32 + 2);
        module.set_name(name);
        module.set_es_module(body.as_bytes());
    }
    let mut output = Vec::new();
    capnp::serialize_packed::write_message(&mut output, &message)?;
    Ok(output)
}

struct Driver(tokio::task::JoinHandle<Result<(), capnp::Error>>);
impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub struct Host {
    _driver: Driver,
    host: isolate_host::Client,
    next: std::cell::Cell<u64>,
    pub bridge: Rc<bridge::State>,
}
pub struct Runtime {
    _host: Rc<Host>,
    pub bridge: Rc<bridge::Worker>,
    pub worker: isolate_protocol::isolate_host_capnp::hosted_isolate::Client,
    service: http::http_service::Client,
    requests: RefCell<tokio::task::JoinSet<()>>,
    request_slots: std::sync::Arc<tokio::sync::Semaphore>,
}
impl Host {
    pub async fn connect(stream: tokio::net::UnixStream) -> Result<Rc<Self>> {
        let (read, write) = stream.compat().split();
        let network = capnp_rpc::twoparty::VatNetwork::new(
            read,
            write,
            capnp_rpc::rpc_twoparty_capnp::Side::Client,
            Default::default(),
        );
        let mut rpc = capnp_rpc::RpcSystem::new(Box::new(network), None);
        let host: isolate_host::Client = rpc.bootstrap(capnp_rpc::rpc_twoparty_capnp::Side::Server);
        let task = tokio::task::spawn_local(rpc);
        let driver = Driver(task);
        let info = host.get_info_request().send().promise.await?;
        let info = info.get()?;
        ensure!(
            info.get_protocol_version() == 1 && info.get_source_format_version() == 1,
            "unsupported host protocol"
        );
        Ok(Rc::new(Self {
            _driver: driver,
            host,
            next: std::cell::Cell::new(0),
            bridge: bridge::State::new(),
        }))
    }
    pub async fn load(self: &Rc<Self>, js: &[u8], storage: std::path::PathBuf) -> Result<Runtime> {
        let id = self.next.get() + 1;
        self.next.set(id);
        let mut start = self.host.start_worker_request();
        start.get().set_worker_id(format!("preview-{id}"));
        start.get().set_worker_source(&bundle(js)?);
        let bridge = bridge::Worker::new(self.bridge.clone());
        let storage = disk::run(move || {
            std::fs::create_dir_all(&storage)?;
            Ok(storage)
        })
        .await?;
        let bindings = storage::Bindings {
            root: storage,
            bridge: Some(bridge.clone()),
        };
        start.get().set_services(capnp_rpc::new_client(bindings));
        let worker = start.send().promise.await?.get()?.get_worker()?;
        let service = worker
            .get_http_service_request()
            .send()
            .promise
            .await?
            .get()?
            .get_service()?;
        Ok(Runtime {
            _host: self.clone(),
            worker,
            bridge,
            service,
            requests: RefCell::new(tokio::task::JoinSet::new()),
            request_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(256)),
        })
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.bridge.stop();
    }
}
impl Runtime {
    pub async fn start(
        stream: tokio::net::UnixStream,
        js: &[u8],
        storage: std::path::PathBuf,
    ) -> Result<Self> {
        Host::connect(stream).await?.load(js, storage).await
    }
    pub async fn validate(&self) -> Result<()> {
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.fetch(http::HttpMethod::Get, "__spki_internal_ready", &[]),
        )
        .await
        .map_err(|_| anyhow::anyhow!("worker initialization exceeded 5 seconds"))??;
        ensure!(
            response.status == 200 && response.body == b"ready",
            "worker initialization failed: {}",
            String::from_utf8_lossy(&response.body)
        );
        Ok(())
    }
    pub async fn fetch(
        &self,
        method: http::HttpMethod,
        path: &str,
        body: &[u8],
    ) -> Result<HttpResponse> {
        self.fetch_with_headers(method, path, body, &[]).await
    }
    pub async fn fetch_with_headers(
        &self,
        method: http::HttpMethod,
        path: &str,
        body: &[u8],
        headers: &[(String, String)],
    ) -> Result<HttpResponse> {
        {
            let mut requests = self.requests.borrow_mut();
            while requests.try_join_next().is_some() {}
        }
        let slot = if path.starts_with("__spki_internal_close_session?id=") {
            None
        } else {
            Some(
                self.request_slots
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| anyhow::anyhow!("too many unfinished worker requests"))?,
            )
        };
        self.worker.keep_alive_request().send().promise.await?;
        let (complete, completed) = futures::channel::oneshot::channel();
        let state = Rc::new(RefCell::new(ResponseState {
            complete: Some(complete),
            ..Default::default()
        }));
        let mut request = self.service.start_request_request();
        let mut metadata = request.get().init_request();
        metadata.set_method(method);
        metadata.set_url(format!("https://preview.invalid/{path}"));
        metadata
            .reborrow()
            .init_body_size()
            .set_fixed(body.len() as u64);
        let mut output = metadata.init_headers(headers.len() as u32);
        for (i, (name, value)) in headers.iter().enumerate() {
            let mut header = output.reborrow().get(i as u32).init_uncommon();
            header.set_name(name);
            header.set_value(value);
        }
        request
            .get()
            .set_context(capnp_rpc::new_client(ResponseContext(state.clone())));
        let response = request.send().promise.await?;
        if !body.is_empty() {
            let stream = response.get()?.get_request_body()?;
            let mut write = stream.write_request();
            write.get().set_bytes(body);
            write.send().await?;
            stream.end_request().send().promise.await?;
        }
        let context = response.get()?.get_context()?;
        // A complete HTTP response is available at EOF (or zero content length).
        // Retain the request context until then and surface earlier execution errors.
        // Some hosts retain their request context beyond a bodyless response.
        tokio::select! {
            completed = completed => { completed?.map_err(anyhow::Error::msg)?; }
            result = context.client.when_resolved() => { result?; }
        }

        self.requests.borrow_mut().spawn_local(async move {
            let _slot = slot;
            let _ = context.client.when_resolved().await;
        });
        let mut state = state.borrow_mut();
        ensure!(state.ended, "response body ended prematurely");
        Ok(HttpResponse {
            status: state
                .status
                .ok_or_else(|| anyhow::anyhow!("no HTTP response"))?,
            headers: std::mem::take(&mut state.headers),
            body: std::mem::take(&mut state.body),
        })
    }
}
pub async fn probe(stream: tokio::net::UnixStream, js: &[u8]) -> Result<(u16, Vec<u8>)> {
    let storage = tempfile::tempdir()?;
    let runtime = Runtime::start(stream, js, storage.path().to_owned()).await?;
    runtime.validate().await?;
    let response = runtime
        .fetch(http::HttpMethod::Get, "probe?test=1", &[])
        .await?;
    runtime.worker.stop_request().send().promise.await?;
    Ok((response.status, response.body))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundle_preserves_javascript_and_version() {
        let js = b"export default {fetch() {return new Response('ok')}}";
        let packed = bundle(js).unwrap();
        let message =
            capnp::serialize_packed::read_message(&mut packed.as_slice(), Default::default())
                .unwrap();
        let source = message
            .get_root::<isolate_worker_source_capnp::isolate_worker_source::Reader>()
            .unwrap();
        assert_eq!(source.get_format_version(), 1);
        match source.get_modules().unwrap().get(1).which().unwrap() {
            isolate_worker_source_capnp::isolate_worker_source::module::EsModule(bytes) => {
                assert_eq!(bytes.unwrap(), js)
            }
            _ => panic!("wrong module kind"),
        }
        assert!(bundle(&vec![0; 8 * 1024 * 1024 + 1]).is_err());
    }
}
