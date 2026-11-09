#![forbid(unsafe_code)]
use anyhow::Result;
use capnp::{capability::FromClientHook, traits::HasTypeId};
use futures::AsyncReadExt;
use isolate_protocol::{
    api_session_capnp::api_session, supervisor_probe_capnp::supervisor,
    web_session_capnp::web_session as web,
};
use tokio_util::compat::TokioAsyncReadCompatExt;
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        args.len() >= 3 && args.len() <= 5,
        "usage: grain-smoke SOCKET PATH [BODYFILE|-] [web|api|api-denied]"
    );
    tokio::task::LocalSet::new()
        .run_until(async {
            let (read, write) = tokio::net::UnixStream::connect(&args[1])
                .await?
                .compat()
                .split();
            let network = capnp_rpc::twoparty::VatNetwork::new(
                read,
                write,
                capnp_rpc::rpc_twoparty_capnp::Side::Client,
                Default::default(),
            );
            let mut rpc = capnp_rpc::RpcSystem::new(Box::new(network), None);
            let supervisor: supervisor::Client =
                rpc.bootstrap(capnp_rpc::rpc_twoparty_capnp::Side::Server);
            let task = tokio::task::spawn_local(rpc);
            let view = supervisor
                .get_main_view_request()
                .send()
                .promise
                .await?
                .get()?
                .get_view()?;
            let mut request = view.new_session_request();
            request
                .get()
                .set_context(capnp_rpc::new_client(SmokeContext));
            let mode = args.get(4).map(String::as_str).unwrap_or("web");
            request.get().set_session_type(if mode.starts_with("api") {
                api_session::Client::TYPE_ID
            } else {
                web::Client::TYPE_ID
            });
            request
                .get()
                .init_user_info()
                .init_display_name()
                .set_default_text("Probe");
            request
                .get()
                .get_session_params()
                .init_as::<web::params::Builder>()
                .set_base_path("https://preview.invalid");
            request
                .get()
                .get_user_info()?
                .init_permissions(1)
                .set(0, mode == "api");
            if std::env::var_os("SPKI_SMOKE_IDENTITY").is_some() {
                request.get().get_user_info()?.set_identity_id(&[0x42; 32]);
            }
            let session = request.send().promise.await?.get()?.get_session()?;
            let session = web::Client::new(session.into_client_hook());
            let body_file = args.get(3).filter(|s| s.as_str() != "-");
            let method = std::env::var("SPKI_SMOKE_METHOD")
                .unwrap_or_else(|_| if body_file.is_some() { "POST" } else { "GET" }.into());
            let mime =
                std::env::var("SPKI_SMOKE_MIME").unwrap_or_else(|_| "application/json".into());
            let encoding = std::env::var("SPKI_SMOKE_ENCODING").unwrap_or_default();
            let mut message = capnp::message::Builder::new_default();
            use capnp::traits::ImbueMut;
            let mut capabilities = Vec::new();
            let mut context = message.init_root::<web::context::Builder>();
            context.imbue_mut(&mut capabilities);
            if let Ok(json) = std::env::var("SPKI_SMOKE_HEADERS") {
                let pairs: Vec<(String, String)> = serde_json::from_str(&json)?;
                let mut headers = context
                    .reborrow()
                    .init_additional_headers(pairs.len() as u32);
                for (i, (name, value)) in pairs.iter().enumerate() {
                    let mut header = headers.reborrow().get(i as u32);
                    header.set_name(name);
                    header.set_value(value);
                }
            }
            if std::env::var_os("SPKI_SMOKE_COOKIE").is_some() {
                let mut cookie = context.reborrow().init_cookies(1).get(0);
                cookie.set_key("test");
                cookie.set_value("cookie-value");
            }
            context.set_response_stream(capnp_rpc::new_client(StreamPrinter));
            let context = context.into_reader();
            macro_rules! body_request {
                ($request:expr) => {{
                    let mut request = $request;
                    request.get().set_path(&args[2]);
                    request.get().set_context(context)?;
                    let mut content = request.get().init_content();
                    content.set_mime_type(&mime);
                    content.set_encoding(&encoding);
                    if let Some(file) = body_file {
                        content.set_content(&std::fs::read(file)?);
                    }
                    request.send().promise.await?
                }};
            }
            let response = match method.as_str() {
                "POST" => body_request!(session.post_request()),
                "PUT" => body_request!(session.put_request()),
                "PATCH" => body_request!(session.patch_request()),
                "DELETE" => {
                    let mut request = session.delete_request();
                    request.get().set_path(&args[2]);
                    request.get().set_context(context)?;
                    request.send().promise.await?
                }
                _ => {
                    let mut request = session.get_request();
                    request.get().set_path(&args[2]);
                    request.get().set_context(context)?;
                    request.get().set_ignore_body(method == "HEAD");
                    request.send().promise.await?
                }
            };
            let response = response.get()?;
            if std::env::var_os("SPKI_SMOKE_INSPECT").is_some() {
                println!("{}", inspect(response)?);
                task.abort();
                return Ok(());
            }
            let web::response::Content(content) = response.which()? else {
                if let web::response::ClientError(error) = response.which()? {
                    anyhow::bail!(
                        "{:?}: {}",
                        error.get_status_code()?,
                        std::str::from_utf8(error.get_non_html_body()?.get_data()?)?
                    );
                }
                anyhow::bail!("expected content");
            };
            if std::env::var_os("SPKI_SMOKE_STREAM").is_some() {
                let web::response::content::body::Stream(handle) = content.get_body().which()?
                else {
                    anyhow::bail!("expected stream");
                };
                let handle = handle?;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                    handle.ping_request().send().promise.await?;
                }
            }
            let web::response::content::body::Bytes(bytes) = content.get_body().which()? else {
                anyhow::bail!("expected bytes");
            };
            println!("{}", std::str::from_utf8(bytes?)?);
            task.abort();
            Ok(())
        })
        .await
}

fn inspect(response: web::response::Reader<'_>) -> Result<serde_json::Value> {
    use serde_json::json;
    let mut value = match response.which()? {
        web::response::Content(content) => {
            let web::response::content::body::Bytes(bytes) = content.get_body().which()? else {
                anyhow::bail!("expected bytes")
            };
            json!({"kind":"content", "status":format!("{:?}", content.get_status_code()?), "body":String::from_utf8_lossy(bytes?), "encoding":content.get_encoding()?.to_str()?, "etag":content.get_e_tag()?.get_value()?.to_str()?})
        }
        web::response::NoContent(_) => json!({"kind":"noContent"}),
        web::response::PreconditionFailed(_) => json!({"kind":"notModified"}),
        web::response::Redirect(redirect) => {
            json!({"kind":"redirect", "location":redirect.get_location()?.to_str()?, "permanent":redirect.get_is_permanent(), "switchToGet":redirect.get_switch_to_get()})
        }
        web::response::ClientError(error) => {
            json!({"kind":"clientError", "status":format!("{:?}", error.get_status_code()?)})
        }
        web::response::ServerError(_) => json!({"kind":"serverError"}),
    };
    let mut cookies = Vec::new();
    for cookie in response.get_set_cookies()? {
        cookies.push(json!({"name":cookie.get_name()?.to_str()?, "value":cookie.get_value()?.to_str()?, "httpOnly":cookie.get_http_only(), "path":cookie.get_path()?.to_str()?}));
    }
    value["cookies"] = json!(cookies);
    let mut headers = Vec::new();
    for header in response.get_additional_headers()? {
        headers.push((
            header.get_name()?.to_str()?.to_owned(),
            header.get_value()?.to_str()?.to_owned(),
        ));
    }
    value["headers"] = json!(headers);
    Ok(value)
}

struct StreamPrinter;
impl isolate_protocol::util_capnp::byte_stream::Server for StreamPrinter {
    async fn write(
        self: std::rc::Rc<Self>,
        params: isolate_protocol::util_capnp::byte_stream::WriteParams,
    ) -> capnp::Result<()> {
        use std::io::Write;
        let mut output = std::io::stdout().lock();
        output
            .write_all(params.get()?.get_data()?)
            .map_err(|e| capnp::Error::failed(e.to_string()))?;
        output
            .flush()
            .map_err(|e| capnp::Error::failed(e.to_string()))?;
        Ok(())
    }
}

struct SmokeContext;
impl isolate_protocol::grain_capnp::session_context::Server for SmokeContext {}
