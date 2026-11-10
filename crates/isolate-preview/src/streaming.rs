//! Application HTTP streaming. A single bounded chunk queue couples host writes to
//! the downstream consumer; dropping the response handle cancels the host request.
use crate::{Runtime, transport::HttpResponse};
use isolate_protocol::{
    byte_stream_capnp::byte_stream as host_stream,
    http_over_capnp_capnp as http,
    util_capnp::{byte_stream, handle},
    web_session_capnp::web_session as web,
};
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
    time::Duration,
};
use tokio::sync::{Mutex, mpsc, oneshot};

const CHUNK: usize = 64 * 1024;
const IDLE: Duration = Duration::from_secs(60);
fn error(message: impl ToString) -> capnp::Error {
    capnp::Error::failed(message.to_string())
}
struct Head {
    response: HttpResponse,
    size: Option<u64>,
    native: bool,
}
enum Chunk {
    Data(Vec<u8>),
    End,
}
struct SinkState {
    count: u64,
    size: Option<u64>,
    ended: bool,
}
struct Sink {
    sender: mpsc::Sender<Chunk>,
    state: Mutex<SinkState>,
}
impl host_stream::Server for Sink {
    async fn get_substream(
        self: Rc<Self>,
        params: host_stream::GetSubstreamParams,
        results: host_stream::GetSubstreamResults,
    ) -> capnp::Result<()> {
        substream(capnp_rpc::new_client_from_rc(self), params, results)
    }

    async fn write(self: Rc<Self>, params: host_stream::WriteParams) -> capnp::Result<()> {
        let p = params.get()?;
        let bytes = p.get_bytes()?;
        let mut state = self.state.lock().await;
        if state.ended
            || state
                .size
                .is_some_and(|size| bytes.len() as u64 > size.saturating_sub(state.count))
        {
            return Err(error(
                "response exceeds declared length or writes after EOF",
            ));
        }
        for chunk in bytes.chunks(CHUNK) {
            tokio::time::timeout(IDLE, self.sender.send(Chunk::Data(chunk.to_vec())))
                .await
                .map_err(|_| error("response consumer stalled"))?
                .map_err(|_| error("response canceled"))?;
        }
        state.count += bytes.len() as u64;
        if state.size == Some(state.count) {
            state.ended = true;
            self.sender
                .send(Chunk::End)
                .await
                .map_err(|_| error("response canceled"))?;
        }
        Ok(())
    }
    async fn end(
        self: Rc<Self>,
        _: host_stream::EndParams,
        _: host_stream::EndResults,
    ) -> capnp::Result<()> {
        let mut state = self.state.lock().await;
        if state.ended {
            return Ok(());
        }
        if state.size.is_some_and(|size| size != state.count) {
            return Err(error("truncated response"));
        }
        state.ended = true;
        self.sender
            .send(Chunk::End)
            .await
            .map_err(|_| error("response canceled"))?;
        Ok(())
    }
}
// workerd uses substreams when piping a request body directly into a response.
// Implement the protocol without requiring C++ path-shortening optimizations.
struct Substream {
    parent: host_stream::Client,
    callback: host_stream::substream_callback::Client,
    limit: u64,
    state: Mutex<SubstreamState>,
}
#[derive(Default)]
struct SubstreamState {
    count: u64,
    ended: bool,
    next: Option<host_stream::Client>,
}
fn substream(
    parent: host_stream::Client,
    params: host_stream::GetSubstreamParams,
    mut results: host_stream::GetSubstreamResults,
) -> capnp::Result<()> {
    let params = params.get()?;
    results
        .get()
        .set_substream(capnp_rpc::new_client(Substream {
            parent,
            callback: params.get_callback()?,
            limit: params.get_limit(),
            state: Mutex::new(SubstreamState::default()),
        }));
    Ok(())
}
impl Substream {
    async fn advance(&self, state: &mut SubstreamState) -> capnp::Result<()> {
        if state.count == self.limit && state.next.is_none() {
            state.next = Some(
                self.callback
                    .reached_limit_request()
                    .send()
                    .promise
                    .await?
                    .get()?
                    .get_next()?,
            );
        }
        Ok(())
    }
}
impl host_stream::Server for Substream {
    async fn write(self: Rc<Self>, params: host_stream::WriteParams) -> capnp::Result<()> {
        let params = params.get()?;
        let bytes = params.get_bytes()?;
        let mut state = self.state.lock().await;
        if state.ended {
            return Err(error("substream already ended"));
        }
        let count = (self.limit - state.count).min(bytes.len() as u64) as usize;
        if count > 0 {
            let mut request = self.parent.write_request();
            request.get().set_bytes(&bytes[..count]);
            request.send().await?;
            state.count += count as u64;
        }
        self.advance(&mut state).await?;
        if count < bytes.len() {
            let mut request = state
                .next
                .as_ref()
                .ok_or_else(|| error("missing next stream"))?
                .write_request();
            request.get().set_bytes(&bytes[count..]);
            request.send().await?;
        }
        Ok(())
    }
    async fn end(
        self: Rc<Self>,
        _: host_stream::EndParams,
        _: host_stream::EndResults,
    ) -> capnp::Result<()> {
        let mut state = self.state.lock().await;
        if state.ended {
            return Err(error("substream already ended"));
        }
        self.advance(&mut state).await?;
        state.ended = true;
        if let Some(next) = &state.next {
            next.end_request().send().promise.await?;
        } else {
            let mut request = self.callback.ended_request();
            request.get().set_byte_count(state.count);
            request.send().promise.await?;
        }
        Ok(())
    }
    async fn get_substream(
        self: Rc<Self>,
        params: host_stream::GetSubstreamParams,
        results: host_stream::GetSubstreamResults,
    ) -> capnp::Result<()> {
        substream(capnp_rpc::new_client_from_rc(self), params, results)
    }
}
struct Context {
    head: RefCell<Option<oneshot::Sender<Head>>>,
    body: RefCell<Option<mpsc::Sender<Chunk>>>,
}
impl http::http_service::client_request_context::Server for Context {
    async fn start_response(
        self: Rc<Self>,
        params: http::http_service::client_request_context::StartResponseParams,
        mut results: http::http_service::client_request_context::StartResponseResults,
    ) -> capnp::Result<()> {
        let p = params.get()?;
        let response = p.get_response()?;
        let mut headers = crate::transport::read_headers(response.get_headers()?)?;
        let mut size = match response.get_body_size().which()? {
            http::http_response::body_size::Fixed(size) => Some(size),
            _ => None,
        };
        // workerd may declare a length in HTTP headers while leaving the RPC
        // body size unknown (for example, a JavaScript ReadableStream).
        for (_, value) in headers.iter().filter(|(name, _)| name == "content-length") {
            let length = value.parse::<u64>().map_err(error)?;
            if size.is_some_and(|size| size != length) {
                return Err(error("conflicting response lengths"));
            }
            size = Some(length);
        }
        let native = headers
            .iter()
            .find(|(name, _)| name == "x-spki-native-body-size")
            .map(|(_, value)| value.parse::<u64>())
            .transpose()
            .map_err(error)?;
        if let Some(length) = native {
            if length > 1024 * 1024 {
                return Err(error("native response exceeds limit"));
            }
            size = Some(length);
        }
        headers.retain(|(name, _)| name != "x-spki-native-body-size");
        let sender = self
            .body
            .borrow_mut()
            .take()
            .ok_or_else(|| error("duplicate response"))?;
        let sink = Sink {
            sender,
            state: Mutex::new(SinkState {
                count: 0,
                size,
                ended: size == Some(0),
            }),
        };
        if size == Some(0) {
            sink.sender.try_send(Chunk::End).map_err(error)?;
        } else {
            results.get().set_body(capnp_rpc::new_client(sink));
        }
        self.head
            .borrow_mut()
            .take()
            .ok_or_else(|| error("duplicate response"))?
            .send(Head {
                response: HttpResponse {
                    status: response.get_status_code(),
                    headers,
                    body: Vec::new(),
                },
                size,
                native: native.is_some(),
            })
            .map_err(|_| error("request canceled"))?;
        Ok(())
    }
}
struct Exchange {
    context: http::http_service::server_request_context::Client,
    _slot: tokio::sync::OwnedSemaphorePermit,
    failed: tokio::sync::watch::Sender<Option<String>>,
}
pub(crate) struct Pending {
    runtime: Weak<Runtime>,
    exchange: Rc<Exchange>,
    head: oneshot::Receiver<Head>,
    body: mpsc::Receiver<Chunk>,
    stopped: tokio::sync::watch::Receiver<bool>,
    failed: tokio::sync::watch::Receiver<Option<String>>,
    upload_task: Option<UploadTask>,
}
impl Runtime {
    async fn start_stream(
        self: &Rc<Self>,
        method: http::HttpMethod,
        path: &str,
        headers: &[(String, String)],
        size: Option<u64>,
    ) -> capnp::Result<(Pending, Option<host_stream::Client>)> {
        while self.requests.borrow_mut().try_join_next().is_some() {}
        let slot = self
            .request_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| error("too many unfinished requests"))?;
        self.worker.keep_alive_request().send().promise.await?;
        let (tx, rx) = oneshot::channel();
        let (body_tx, body_rx) = mpsc::channel(1);
        let mut request = self.service.start_request_request();
        let mut metadata = request.get().init_request();
        metadata.set_method(method);
        metadata.set_url(format!("https://preview.invalid/{path}"));
        match size {
            Some(size) => metadata.reborrow().init_body_size().set_fixed(size),
            None => metadata.reborrow().init_body_size().set_unknown(()),
        }
        let mut list = metadata.init_headers(headers.len() as u32);
        for (i, (name, value)) in headers.iter().enumerate() {
            let mut h = list.reborrow().get(i as u32).init_uncommon();
            h.set_name(name);
            h.set_value(value);
        }
        request.get().set_context(capnp_rpc::new_client(Context {
            head: RefCell::new(Some(tx)),
            body: RefCell::new(Some(body_tx)),
        }));
        let result = tokio::time::timeout(IDLE, request.send().promise)
            .await
            .map_err(|_| error("request start timed out"))??;
        let result = result.get()?;
        let upload = if size == Some(0) {
            None
        } else {
            Some(result.get_request_body()?)
        };
        let (failed, failure) = tokio::sync::watch::channel(None);
        Ok((
            Pending {
                runtime: Rc::downgrade(self),
                exchange: Rc::new(Exchange {
                    context: result.get_context()?,
                    _slot: slot,
                    failed,
                }),
                head: rx,
                body: body_rx,
                stopped: self.bridge.stopped.subscribe(),
                failed: failure,
                upload_task: None,
            },
            upload,
        ))
    }
}
impl Pending {
    async fn next(&mut self) -> capnp::Result<Chunk> {
        if *self.stopped.borrow() {
            return Err(error("worker replaced"));
        }
        if let Some(failure) = self.failed.borrow().as_ref() {
            return Err(error(failure));
        }
        let exchange = self.exchange.clone();
        tokio::select! {
            _=self.stopped.changed()=>Err(error("worker replaced")),
            _=self.failed.changed()=>Err(error("upload failed")),
            chunk=tokio::time::timeout(IDLE,self.body.recv())=>chunk.map_err(|_|error("response stalled"))?.ok_or_else(||error("response ended without EOF")),
            result=exchange.context.client.when_resolved()=> {
                result?;
                tokio::time::timeout(IDLE,self.body.recv()).await.map_err(|_|error("response stalled"))?.ok_or_else(||error("response ended without EOF"))
            }
        }
    }
    fn retain_native(&self) {
        if let Some(runtime) = self.runtime.upgrade() {
            let exchange = self.exchange.clone();
            runtime.requests.borrow_mut().spawn_local(async move {
                let _ = exchange.context.client.when_resolved().await;
            });
        }
    }
    async fn respond(
        mut self,
        target: byte_stream::Client,
        method: http::HttpMethod,
        revision: Option<String>,
        mut results: web::GetResults,
    ) -> capnp::Result<()> {
        let exchange = self.exchange.clone();
        if let Some(failure) = self.failed.borrow().as_ref() {
            return Err(error(failure));
        }
        let mut head = tokio::select! {
            _=self.failed.changed()=>return Err(error("upload failed")),
            _=self.stopped.changed()=>return Err(error("worker replaced")),
            head=tokio::time::timeout(IDLE,&mut self.head)=>head.map_err(|_|error("response headers timed out"))?.map_err(|_|error("no response headers"))?,
            result=exchange.context.client.when_resolved()=> {result?;tokio::time::timeout(IDLE,&mut self.head).await.map_err(|_|error("response headers timed out"))?.map_err(|_|error("no response headers"))?}
        };
        if method == http::HttpMethod::Head {
            crate::http_adapter::write_response(results.get(), &head.response)?;
            return Ok(());
        }
        if !matches!(head.response.status, 200 | 201 | 202 | 206 | 207) {
            while let Chunk::Data(bytes) = self.next().await? {
                if head.response.body.len() + bytes.len() > 1024 * 1024 {
                    return Err(error("non-content response exceeds 1 MiB"));
                }
                head.response.body.extend_from_slice(&bytes);
            }
            crate::http_adapter::write_response(results.get(), &head.response)?;
            if head.native {
                self.retain_native();
            }
            return Ok(());
        }
        let suffix = revision
            .filter(|_| head.response.header("content-encoding").is_empty())
            .map(|revision| {
                crate::reload::inject(
                    head.response.status,
                    head.response.header("content-type"),
                    Vec::new(),
                    &revision,
                )
            })
            .unwrap_or_default();
        crate::http_adapter::write_response(results.get(), &head.response)?;
        // A fixed-length response may complete at the gateway on the last write,
        // before done(). Retain native callbacks before any body bytes are sent.
        if head.native {
            self.retain_native();
        }
        let task = tokio::task::spawn_local(async move {
            let result: capnp::Result<()> = async {
                if let Some(size) = head.size.filter(|_| suffix.is_empty()) {
                    let mut req = target.expect_size_request();
                    req.get().set_size(size);
                    let _ = tokio::time::timeout(IDLE, req.send().promise).await;
                }
                let mut utf8 = Utf8::default();
                while let Chunk::Data(bytes) = self.next().await? {
                    if !suffix.is_empty() {
                        utf8.push(&bytes);
                    }
                    let mut request = target.write_request();
                    request.get().set_data(&bytes);
                    tokio::time::timeout(IDLE, request.send())
                        .await
                        .map_err(|_| error("response consumer stalled"))??;
                }
                if !suffix.is_empty() && !utf8.invalid && utf8.tail.is_empty() {
                    let mut req = target.write_request();
                    req.get().set_data(&suffix);
                    tokio::time::timeout(IDLE, req.send())
                        .await
                        .map_err(|_| error("response consumer stalled"))??;
                }
                tokio::time::timeout(IDLE, target.done_request().send().promise)
                    .await
                    .map_err(|_| error("response consumer stalled"))??;
                Ok(())
            }
            .await;
            // Dropping target without done() tells Sandstorm that this response was interrupted.
            if let Err(error) = result {
                eprintln!("HTTP response stream failed: {error}");
            }
        });
        if let web::response::Content(content) = results.get().which()? {
            content
                .init_body()
                .set_stream(capnp_rpc::new_client(Download { task }));
        }
        Ok(())
    }
}
struct Download {
    task: tokio::task::JoinHandle<()>,
}
impl handle::Server for Download {
    async fn ping(
        self: Rc<Self>,
        _: handle::PingParams,
        _: handle::PingResults,
    ) -> capnp::Result<()> {
        if self.task.is_finished() {
            Err(error("stream finished"))
        } else {
            Ok(())
        }
    }
}
impl Drop for Download {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) struct Request {
    pub method: http::HttpMethod,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub revision: Option<String>,
}
struct UploadTask(tokio::task::JoinHandle<()>);
impl Drop for UploadTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub(crate) async fn fetch(
    runtime: &Rc<Runtime>,
    request: Request,
    body: &[u8],
    target: byte_stream::Client,
    results: web::GetResults,
) -> capnp::Result<()> {
    let (mut pending, upload) = runtime
        .start_stream(
            request.method,
            &request.path,
            &request.headers,
            Some(body.len() as u64),
        )
        .await?;
    if let Some(upload) = upload {
        let body = body.to_vec();
        let failed = pending.exchange.failed.clone();
        pending.upload_task = Some(UploadTask(tokio::task::spawn_local(async move {
            let result: capnp::Result<()> = async {
                for chunk in body.chunks(CHUNK) {
                    let mut req = upload.write_request();
                    req.get().set_bytes(chunk);
                    tokio::time::timeout(IDLE, req.send())
                        .await
                        .map_err(|_| error("upload stalled"))??;
                }
                tokio::time::timeout(IDLE, upload.end_request().send().promise)
                    .await
                    .map_err(|_| error("upload stalled"))??;
                Ok(())
            }
            .await;
            if let Err(error) = result {
                failed.send_replace(Some(error.to_string()));
            }
        })));
    }
    pending
        .respond(target, request.method, request.revision, results)
        .await
}

struct Upload {
    stream: RefCell<Option<host_stream::Client>>,
    exchange: Rc<Exchange>,
    state: Mutex<SinkState>,
    pending: RefCell<Option<Pending>>,
    target: byte_stream::Client,
    method: http::HttpMethod,
    revision: Option<String>,
}
impl byte_stream::Server for Upload {
    async fn write(self: Rc<Self>, params: byte_stream::WriteParams) -> capnp::Result<()> {
        let p = params.get()?;
        let data = p.get_data()?;
        let mut state = self.state.lock().await;
        if state.ended
            || state
                .size
                .is_some_and(|size| data.len() as u64 > size.saturating_sub(state.count))
        {
            return Err(error("invalid upload length or write after EOF"));
        }
        let stream = self
            .stream
            .borrow()
            .clone()
            .ok_or_else(|| error("upload already ended"))?;
        for chunk in data.chunks(CHUNK) {
            let mut req = stream.write_request();
            req.get().set_bytes(chunk);
            tokio::time::timeout(IDLE, req.send())
                .await
                .map_err(|_| error("upload stalled"))??;
        }
        state.count += data.len() as u64;
        Ok(())
    }
    async fn done(
        self: Rc<Self>,
        _: byte_stream::DoneParams,
        _: byte_stream::DoneResults,
    ) -> capnp::Result<()> {
        let mut state = self.state.lock().await;
        if state.ended {
            return Err(error("upload already ended"));
        }
        if state.size.is_some_and(|size| size != state.count) {
            return Err(error("truncated upload"));
        }
        state.ended = true;
        // Release the completed host upload even if the caller keeps RequestStream.
        // Otherwise workerd's shortened pipe can keep the response open after EOF.
        let stream = self
            .stream
            .borrow_mut()
            .take()
            .ok_or_else(|| error("upload already ended"))?;
        tokio::time::timeout(IDLE, stream.end_request().send().promise)
            .await
            .map_err(|_| error("upload stalled"))??;
        Ok(())
    }
    async fn expect_size(
        self: Rc<Self>,
        params: byte_stream::ExpectSizeParams,
        _: byte_stream::ExpectSizeResults,
    ) -> capnp::Result<()> {
        let mut state = self.state.lock().await;
        let size = state
            .count
            .checked_add(params.get()?.get_size())
            .ok_or_else(|| error("upload length overflow"))?;
        if state.ended || state.size.is_some_and(|old| old != size) {
            return Err(error("inconsistent upload size"));
        }
        state.size = Some(size);
        Ok(())
    }
}
impl web::request_stream::Server for Upload {
    async fn get_response(
        self: Rc<Self>,
        _: web::request_stream::GetResponseParams,
        results: web::request_stream::GetResponseResults,
    ) -> capnp::Result<()> {
        let pending = self
            .pending
            .borrow_mut()
            .take()
            .ok_or_else(|| error("response already requested"))?;
        let _exchange = &self.exchange;
        pending
            .respond(
                self.target.clone(),
                self.method,
                self.revision.clone(),
                results,
            )
            .await
    }
}
pub(crate) async fn upload(
    runtime: &Rc<Runtime>,
    request: Request,
    size: u64,
    target: byte_stream::Client,
) -> capnp::Result<web::request_stream::Client> {
    let size = if size == 0 { None } else { Some(size) };
    let (pending, stream) = runtime
        .start_stream(request.method, &request.path, &request.headers, size)
        .await?;
    Ok(capnp_rpc::new_client(Upload {
        stream: RefCell::new(Some(stream.ok_or_else(|| error("missing upload stream"))?)),
        exchange: pending.exchange.clone(),
        state: Mutex::new(SinkState {
            count: 0,
            size,
            ended: false,
        }),
        pending: RefCell::new(Some(pending)),
        target,
        method: request.method,
        revision: request.revision,
    }))
}

pub(crate) fn content_headers(
    headers: &mut Vec<(String, String)>,
    mime: &str,
    encoding: &str,
) -> capnp::Result<()> {
    for (name, value) in [("content-type", mime), ("content-encoding", encoding)] {
        if !value.is_empty() {
            if !crate::transport::valid_header(name, value) {
                return Err(error("invalid content header"));
            }
            headers.push((name.into(), value.into()));
        }
    }
    Ok(())
}

#[derive(Default)]
struct Utf8 {
    tail: Vec<u8>,
    invalid: bool,
}
impl Utf8 {
    fn push(&mut self, bytes: &[u8]) {
        if self.invalid {
            return;
        }
        let joined;
        let bytes = if self.tail.is_empty() {
            bytes
        } else {
            joined = [self.tail.as_slice(), bytes].concat();
            self.tail.clear();
            &joined
        };
        if let Err(e) = std::str::from_utf8(bytes) {
            if e.error_len().is_some() {
                self.invalid = true;
            } else {
                self.tail.extend_from_slice(&bytes[e.valid_up_to()..]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sink(size: Option<u64>) -> (host_stream::Client, mpsc::Receiver<Chunk>) {
        let (sender, receiver) = mpsc::channel(1);
        (
            capnp_rpc::new_client(Sink {
                sender,
                state: Mutex::new(SinkState {
                    count: 0,
                    size,
                    ended: false,
                }),
            }),
            receiver,
        )
    }
    #[tokio::test(flavor = "current_thread")]
    async fn backpressure_and_consumer_cancellation() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (client, mut receiver) = sink(Some((3 * CHUNK) as u64));
                let mut req = client.write_request();
                req.get().set_bytes(&vec![9; 3 * CHUNK]);
                let writing = tokio::task::spawn_local(async move { req.send().await });
                tokio::time::sleep(Duration::from_millis(10)).await;
                assert!(!writing.is_finished());
                assert_eq!(receiver.len(), 1);
                for _ in 0..3 {
                    let Some(Chunk::Data(bytes)) = receiver.recv().await else {
                        panic!("missing bytes")
                    };
                    assert_eq!(bytes.len(), CHUNK);
                }
                assert!(matches!(receiver.recv().await, Some(Chunk::End)));
                writing.await.unwrap().unwrap();
                client.end_request().send().promise.await.unwrap();
                let mut late = client.write_request();
                late.get().set_bytes(b"late");
                assert!(late.send().await.is_err());
                let (client, mut receiver) = sink(None);
                let mut req = client.write_request();
                req.get().set_bytes(&vec![1; 3 * CHUNK]);
                let writing = tokio::task::spawn_local(async move { req.send().await });
                assert!(matches!(receiver.recv().await, Some(Chunk::Data(_))));
                drop(receiver);
                assert!(writing.await.unwrap().is_err());
            })
            .await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn truncated_and_oversized_responses_fail() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (client, _receiver) = sink(Some(3));
                assert!(client.end_request().send().promise.await.is_err());
                let (client, _receiver) = sink(Some(3));
                let mut request = client.write_request();
                request.get().set_bytes(b"four");
                assert!(request.send().await.is_err());
            })
            .await;
    }
    struct Callback {
        next: host_stream::Client,
        ended: Rc<std::cell::Cell<Option<u64>>>,
        reached: Rc<std::cell::Cell<bool>>,
    }
    impl host_stream::substream_callback::Server for Callback {
        async fn ended(
            self: Rc<Self>,
            params: host_stream::substream_callback::EndedParams,
            _: host_stream::substream_callback::EndedResults,
        ) -> capnp::Result<()> {
            self.ended.set(Some(params.get()?.get_byte_count()));
            Ok(())
        }
        async fn reached_limit(
            self: Rc<Self>,
            _: host_stream::substream_callback::ReachedLimitParams,
            mut results: host_stream::substream_callback::ReachedLimitResults,
        ) -> capnp::Result<()> {
            self.reached.set(true);
            results.get().set_next(self.next.clone());
            Ok(())
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn substreams_split_at_limit_and_end_without_closing_parent() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for limit in [0, 2, 10] {
                    let (parent, mut original) = sink(None);
                    let (next, mut overflow) = sink(None);
                    let ended = Rc::new(std::cell::Cell::new(None));
                    let reached = Rc::new(std::cell::Cell::new(false));
                    let mut request = parent.get_substream_request();
                    request.get().set_limit(limit);
                    request.get().set_callback(capnp_rpc::new_client(Callback {
                        next,
                        ended: ended.clone(),
                        reached: reached.clone(),
                    }));
                    let sub = request
                        .send()
                        .promise
                        .await
                        .unwrap()
                        .get()
                        .unwrap()
                        .get_substream()
                        .unwrap();
                    let mut write = sub.write_request();
                    write.get().set_bytes(b"abcde");
                    write.send().await.unwrap();
                    let split = (limit as usize).min(5);
                    if split > 0 {
                        let Some(Chunk::Data(bytes)) = original.recv().await else {
                            panic!("missing parent bytes")
                        };
                        assert_eq!(bytes, &b"abcde"[..split]);
                    }
                    if split < 5 {
                        let Some(Chunk::Data(bytes)) = overflow.recv().await else {
                            panic!("missing overflow bytes")
                        };
                        assert_eq!(bytes, &b"abcde"[split..]);
                    }
                    sub.end_request().send().promise.await.unwrap();
                    if limit <= 5 {
                        assert!(reached.get());
                        assert!(matches!(overflow.recv().await, Some(Chunk::End)));
                        assert_eq!(ended.get(), None);
                    } else {
                        assert!(!reached.get());
                        assert_eq!(ended.get(), Some(5));
                        let mut write = parent.write_request();
                        write.get().set_bytes(b"still open");
                        write.send().await.unwrap();
                        assert!(matches!(original.recv().await, Some(Chunk::Data(_))));
                    }
                }
            })
            .await;
    }
    #[test]
    fn utf8_validation_spans_chunk_boundaries() {
        let mut validator = Utf8::default();
        for byte in "a🍒b".as_bytes() {
            validator.push(&[*byte]);
        }
        assert!(!validator.invalid && validator.tail.is_empty());
        validator.push(&[255]);
        assert!(validator.invalid);
        let mut validator = Utf8::default();
        validator.push(&[0xf0, 0x9f]);
        assert!(!validator.tail.is_empty());
        validator.push(b"not a continuation");
        assert!(validator.invalid);
    }
}
