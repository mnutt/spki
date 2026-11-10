//! Bounded HTTP-over-Cap’n-Proto transport; independent of worker lifecycle.
use anyhow::Result;
use isolate_protocol::{byte_stream_capnp::byte_stream, http_over_capnp_capnp as http};
use std::{cell::RefCell, rc::Rc};

#[derive(Debug, Default)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
impl HttpResponse {
    pub fn header(&self, name: &str) -> &str {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .unwrap_or("")
    }
}
const MAX_RESPONSE: usize = 1024 * 1024;

#[derive(Default)]
pub(crate) struct ResponseState {
    pub(crate) status: Option<u16>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
    pub(crate) ended: bool,
    pub(crate) expected: Option<usize>,
    pub(crate) complete: Option<futures::channel::oneshot::Sender<Result<(), String>>>,
}
pub(crate) struct Body(pub(crate) Rc<RefCell<ResponseState>>);
impl byte_stream::Server for Body {
    async fn write(self: Rc<Self>, params: byte_stream::WriteParams) -> capnp::Result<()> {
        let params = params.get()?;
        let bytes = params.get_bytes()?;
        let mut state = self.0.borrow_mut();
        if state.ended || bytes.len() > MAX_RESPONSE - state.body.len() {
            if let Some(complete) = state.complete.take() {
                let _ = complete.send(Err("invalid or oversized response body".into()));
            }
            return Err(capnp::Error::failed(
                "invalid or oversized response body".into(),
            ));
        }
        if let Some(expected) = state.expected
            && bytes.len() > expected.saturating_sub(state.body.len())
        {
            if let Some(complete) = state.complete.take() {
                let _ = complete.send(Err("body exceeds declared length".into()));
            }
            return Err(capnp::Error::failed("body exceeds declared length".into()));
        }
        state.body.extend_from_slice(bytes);
        if state.expected == Some(state.body.len()) {
            state.ended = true;
            if let Some(complete) = state.complete.take() {
                let _ = complete.send(Ok(()));
            }
        }
        Ok(())
    }
    async fn end(
        self: Rc<Self>,
        _: byte_stream::EndParams,
        _: byte_stream::EndResults,
    ) -> capnp::Result<()> {
        let mut state = self.0.borrow_mut();
        if state.ended {
            return Ok(());
        }
        if state.expected.is_some_and(|size| size != state.body.len()) {
            if let Some(complete) = state.complete.take() {
                let _ = complete.send(Err("body ended before declared length".into()));
            }
            return Err(capnp::Error::failed(
                "body ended before declared length".into(),
            ));
        }
        state.ended = true;
        if let Some(complete) = state.complete.take() {
            let _ = complete.send(Ok(()));
        }
        Ok(())
    }
}
pub(crate) struct ResponseContext(pub(crate) Rc<RefCell<ResponseState>>);
impl http::http_service::client_request_context::Server for ResponseContext {
    async fn start_response(
        self: Rc<Self>,
        params: http::http_service::client_request_context::StartResponseParams,
        mut results: http::http_service::client_request_context::StartResponseResults,
    ) -> capnp::Result<()> {
        let params = params.get()?;
        let response = params.get_response()?;
        let mut state = self.0.borrow_mut();
        if state.status.is_some() {
            return Err(capnp::Error::failed("duplicate response".into()));
        }
        state.status = Some(response.get_status_code());
        state.headers = read_headers(response.get_headers()?)?;

        if let http::http_response::body_size::Fixed(size) = response.get_body_size().which()? {
            if size > MAX_RESPONSE as u64 {
                if let Some(complete) = state.complete.take() {
                    let _ = complete.send(Err("response too large".into()));
                }
                return Err(capnp::Error::failed("response too large".into()));
            }
            state.expected = Some(size as usize);
            if size == 0 {
                state.ended = true;
            }
        }
        if let Some((_, value)) = state
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("x-spki-native-body-size"))
        {
            let size: usize = value
                .parse()
                .map_err(|_| capnp::Error::failed("invalid native response size".into()))?;
            if size > MAX_RESPONSE {
                return Err(capnp::Error::failed("native response exceeds limit".into()));
            }
            state.expected = Some(size);
            state.ended = size == 0;
        }
        state
            .headers
            .retain(|(name, _)| !name.eq_ignore_ascii_case("x-spki-native-body-size"));
        if state.ended
            && let Some(complete) = state.complete.take()
        {
            let _ = complete.send(Ok(()));
        }
        if !state.ended {
            results
                .get()
                .set_body(capnp_rpc::new_client(Body(self.0.clone())));
        }
        Ok(())
    }
}

pub(crate) fn read_headers(
    headers: capnp::struct_list::Reader<'_, http::http_header::Owned>,
) -> capnp::Result<Vec<(String, String)>> {
    let mut output = Vec::new();
    let mut size = 0;
    for header in headers {
        let (name, value) = match header.which()? {
            http::http_header::Uncommon(pair) => {
                let pair = pair?;
                (
                    pair.get_name()?.to_str()?.to_ascii_lowercase(),
                    pair.get_value()?.to_str()?.to_owned(),
                )
            }
            http::http_header::Common(common) => {
                let name = match common.get_name()? {
                    http::CommonHeaderName::AcceptCharset => "accept-charset",
                    http::CommonHeaderName::AcceptEncoding => "accept-encoding",
                    http::CommonHeaderName::AcceptLanguage => "accept-language",
                    http::CommonHeaderName::AcceptRanges => "accept-ranges",
                    http::CommonHeaderName::Accept => "accept",
                    http::CommonHeaderName::AccessControlAllowOrigin => {
                        "access-control-allow-origin"
                    }
                    http::CommonHeaderName::Age => "age",
                    http::CommonHeaderName::Allow => "allow",
                    http::CommonHeaderName::Authorization => "authorization",
                    http::CommonHeaderName::CacheControl => "cache-control",
                    http::CommonHeaderName::ContentDisposition => "content-disposition",
                    http::CommonHeaderName::ContentEncoding => "content-encoding",
                    http::CommonHeaderName::ContentLanguage => "content-language",
                    http::CommonHeaderName::ContentLength => "content-length",
                    http::CommonHeaderName::ContentLocation => "content-location",
                    http::CommonHeaderName::ContentRange => "content-range",
                    http::CommonHeaderName::ContentType => "content-type",
                    http::CommonHeaderName::Cookie => "cookie",
                    http::CommonHeaderName::Date => "date",
                    http::CommonHeaderName::Etag => "etag",
                    http::CommonHeaderName::Expect => "expect",
                    http::CommonHeaderName::Expires => "expires",
                    http::CommonHeaderName::From => "from",
                    http::CommonHeaderName::Host => "host",
                    http::CommonHeaderName::IfMatch => "if-match",
                    http::CommonHeaderName::IfModifiedSince => "if-modified-since",
                    http::CommonHeaderName::IfNoneMatch => "if-none-match",
                    http::CommonHeaderName::IfRange => "if-range",
                    http::CommonHeaderName::IfUnmodifiedSince => "if-unmodified-since",
                    http::CommonHeaderName::LastModified => "last-modified",
                    http::CommonHeaderName::Link => "link",
                    http::CommonHeaderName::Location => "location",
                    http::CommonHeaderName::MaxForwards => "max-forwards",
                    http::CommonHeaderName::ProxyAuthenticate => "proxy-authenticate",
                    http::CommonHeaderName::ProxyAuthorization => "proxy-authorization",
                    http::CommonHeaderName::Range => "range",
                    http::CommonHeaderName::Referer => "referer",
                    http::CommonHeaderName::Refresh => "refresh",
                    http::CommonHeaderName::RetryAfter => "retry-after",
                    http::CommonHeaderName::Server => "server",
                    http::CommonHeaderName::SetCookie => "set-cookie",
                    http::CommonHeaderName::StrictTransportSecurity => "strict-transport-security",
                    http::CommonHeaderName::TransferEncoding => "transfer-encoding",
                    http::CommonHeaderName::UserAgent => "user-agent",
                    http::CommonHeaderName::Vary => "vary",
                    http::CommonHeaderName::Via => "via",
                    http::CommonHeaderName::WwwAuthenticate => "www-authenticate",
                    http::CommonHeaderName::Invalid => {
                        return Err(capnp::Error::failed("invalid header name".into()));
                    }
                };
                let value = match common.which()? {
                    http::http_header::common::Value(value) => value?.to_str()?.to_owned(),
                    http::http_header::common::CommonValue(value) => match value? {
                        http::CommonHeaderValue::GzipDeflate => "gzip, deflate".into(),
                        _ => return Err(capnp::Error::failed("invalid header value".into())),
                    },
                };
                (name.to_owned(), value)
            }
        };
        size += name.len() + value.len();
        if size > 65536 || output.len() >= 256 || !valid_header(&name, &value) {
            return Err(capnp::Error::failed("invalid or oversized headers".into()));
        }
        output.push((name, value));
    }
    Ok(output)
}
pub(crate) fn valid_header(name: &str, value: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        && !value.bytes().any(|b| b == 127 || (b < 32 && b != b'\t'))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_length_upload_completes_without_end_and_rejects_truncation() {
        futures::executor::block_on(async {
            let (tx, rx) = futures::channel::oneshot::channel();
            let state = Rc::new(RefCell::new(ResponseState {
                expected: Some(3),
                complete: Some(tx),
                ..Default::default()
            }));
            let client: byte_stream::Client = capnp_rpc::new_client(Body(state.clone()));
            let mut write = client.write_request();
            write.get().set_bytes(b"123");
            write.send().await.unwrap();
            rx.await.unwrap().unwrap();
            state.borrow_mut().body.clear(); // The response consumer has already taken the body.
            client.end_request().send().promise.await.unwrap();
            let state = Rc::new(RefCell::new(ResponseState {
                expected: Some(3),
                ..Default::default()
            }));
            let client: byte_stream::Client = capnp_rpc::new_client(Body(state));
            assert!(client.end_request().send().promise.await.is_err());
        });
    }
    #[test]
    fn response_sink_rejects_overflow_and_writes_after_end() {
        futures::executor::block_on(async {
            let state = Rc::new(RefCell::new(ResponseState::default()));
            let client: byte_stream::Client = capnp_rpc::new_client(Body(state.clone()));
            let mut request = client.write_request();
            request.get().set_bytes(b"hello");
            request.send().await.unwrap();
            assert_eq!(state.borrow().body, b"hello");
            let mut request = client.write_request();
            request.get().set_bytes(&vec![0; MAX_RESPONSE]);
            assert!(request.send().await.is_err());
            // A failed streaming call poisons that capability; use a fresh stream
            // to independently test clean EOF and rejection of later writes.
            let state = Rc::new(RefCell::new(ResponseState::default()));
            let client: byte_stream::Client = capnp_rpc::new_client(Body(state));
            client.end_request().send().promise.await.unwrap();
            let mut request = client.write_request();
            request.get().set_bytes(b"late");
            assert!(request.send().await.is_err());
        });
    }
}
