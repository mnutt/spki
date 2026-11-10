//! Durable, anonymous HTTP capabilities restricted to an application path prefix.
use crate::controller::Controller;
use capnp::capability::{Client, FromClientHook};
use isolate_protocol::{
    api_session_capnp::api_session,
    grain_capnp::{app_persistent, ui_session},
    http_over_capnp_capnp::HttpMethod,
    route_session_capnp::{route_object_id, route_session},
    web_session_capnp::web_session as web,
};
use std::rc::{Rc, Weak};

fn invalid(message: &str) -> capnp::Error {
    capnp::Error::failed(message.into())
}

// Validate decoded paths too: URL parsers normalize encoded dots and backslashes.
// Reject encoded separators and nested escaping rather than allowing ambiguous scopes.
pub(crate) fn validate_path(path: &str) -> capnp::Result<()> {
    if path.len() > 8192 || path.chars().any(|c| c.is_control()) || path.contains(['#', '\\']) {
        return Err(invalid("invalid capability path"));
    }
    let raw = path.split('?').next().unwrap_or(path);
    let decoded = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .map_err(|_| invalid("invalid path encoding"))?;
    if decoded.contains(['%', '\\', '?', '#', ':'])
        || decoded.chars().any(|c| c.is_control())
        || decoded.matches('/').count() != raw.matches('/').count()
        || decoded.split('/').any(|part| part == "." || part == "..")
    {
        return Err(invalid("capability path must be canonical and relative"));
    }
    Ok(())
}
fn prefix(value: &str) -> capnp::Result<String> {
    validate_path(value)?;
    if value.len() > 1024 || value.contains('?') || (!value.is_empty() && !value.starts_with('/')) {
        return Err(invalid("invalid capability path prefix"));
    }
    Ok(value.trim_end_matches('/').to_owned())
}
fn scoped_path(prefix: &str, path: &str) -> capnp::Result<String> {
    validate_path(path)?;
    let combined = format!(
        "{}/{}",
        prefix.trim_matches('/'),
        path.trim_start_matches('/')
    );
    let combined = combined.trim_start_matches('/').to_owned();
    let decoded = percent_encoding::percent_decode_str(&combined)
        .decode_utf8()
        .map_err(|_| invalid("invalid path encoding"))?;
    if decoded.starts_with("__spki")
        || decoded.starts_with("__sandstorm")
        || decoded.starts_with("capnp-es/")
    {
        return Err(invalid("runtime routes are not application capabilities"));
    }
    Ok(combined)
}

struct Route {
    project_id: String,
    controller: Weak<Controller>,
    native: String,
    prefix: String,
    persistent: bool,
}
pub(crate) fn create(
    controller: Weak<Controller>,
    native: &str,
    path: &str,
    persistent: bool,
) -> capnp::Result<Client> {
    if !matches!(native, "webSession" | "apiSession") {
        return Err(invalid("unsupported route interface"));
    }
    let prefix = prefix(path)?;
    scoped_path(&prefix, "")?;
    let cap: route_session::Client = capnp_rpc::new_client(Route {
        project_id: controller
            .upgrade()
            .map(|c| c.project_id.clone())
            .unwrap_or_default(),
        controller,
        native: native.into(),
        prefix,
        persistent,
    });
    Ok(Client::new(cap.into_client_hook()))
}
pub(crate) fn restore(
    controller: Weak<Controller>,
    id: capnp::any_pointer::Reader<'_>,
) -> capnp::Result<Client> {
    let id = id.get_as::<route_object_id::Reader>()?;
    if !matches!(
        id.get_format()?.to_str()?,
        "spki-route-v1" | "spki-route-v2"
    ) {
        return Err(invalid("unknown persistent object"));
    }
    let project_id = id.get_project_id()?.to_str()?;
    if controller
        .upgrade()
        .map(|c| c.project_id.clone())
        .unwrap_or_default()
        != project_id
    {
        return Err(invalid("persistent route belongs to another project"));
    }
    create(
        controller,
        id.get_native_interface()?.to_str()?,
        id.get_path_prefix()?.to_str()?,
        true,
    )
}
impl route_session::Server for Route {}
impl ui_session::Server for Route {}
impl api_session::Server for Route {}
impl app_persistent::Server<capnp::any_pointer::Owned> for Route {
    async fn save(
        self: Rc<Self>,
        _: app_persistent::SaveParams<capnp::any_pointer::Owned>,
        mut results: app_persistent::SaveResults<capnp::any_pointer::Owned>,
    ) -> capnp::Result<()> {
        if !self.persistent {
            return Err(invalid("capability is not persistent"));
        }
        let mut result = results.get();
        let mut id = result
            .reborrow()
            .get_object_id()?
            .init_as::<route_object_id::Builder>();
        id.set_format(if self.project_id.is_empty() {
            "spki-route-v1"
        } else {
            "spki-route-v2"
        });
        id.set_project_id(&self.project_id);
        id.set_native_interface(&self.native);
        id.set_path_prefix(&self.prefix);
        result
            .init_label()
            .set_default_text(format!("{} {}", self.native, self.prefix));
        Ok(())
    }
}
impl Route {
    async fn respond(
        &self,
        method: HttpMethod,
        path: String,
        body: Vec<u8>,
        mut headers: Vec<(String, String)>,
        stream: Option<isolate_protocol::util_capnp::byte_stream::Client>,
        mut results: web::GetResults,
    ) -> capnp::Result<()> {
        let path = scoped_path(&self.prefix, &path)?;
        // A route is authority to this path, not the identity of its creator or caller.
        headers.push((
            "x-sandstorm-session-type".into(),
            if self.native == "apiSession" {
                "api"
            } else {
                "web"
            }
            .into(),
        ));
        let controller = self
            .controller
            .upgrade()
            .ok_or_else(|| invalid("grain closed"))?;
        if controller.is_recovering() {
            return Err(invalid("application awaits recovery"));
        }
        let runtime = controller.snapshot().1;
        if let Some(stream) = stream {
            return crate::streaming::fetch(
                &runtime,
                crate::streaming::Request {
                    method,
                    path,
                    headers,
                    revision: None,
                },
                &body,
                stream,
                results,
            )
            .await;
        }
        let mut response = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            runtime.fetch_with_headers(method, &path, &body, &headers),
        )
        .await
        .map_err(|_| invalid("route request timed out"))?
        .map_err(|e| invalid(&e.to_string()))?;
        if method == HttpMethod::Head {
            response.body.clear();
        }
        crate::http_adapter::write_response(results.get(), &response)
    }
}
fn upload_headers(
    data: &[u8],
    mime: &str,
    encoding: &str,
    context: web::context::Reader<'_>,
) -> capnp::Result<Vec<(String, String)>> {
    if data.len() > 9 * 1024 * 1024 {
        return Err(invalid("upload too large"));
    }
    let mut headers = crate::http_adapter::request_headers(context)?;
    for (name, value) in [("content-type", mime), ("content-encoding", encoding)] {
        if !value.is_empty() {
            headers.push((name.into(), value.to_owned()));
        }
    }
    Ok(headers)
}
impl web::Server for Route {
    async fn post_streaming(
        self: Rc<Self>,
        params: web::PostStreamingParams,
        mut results: web::PostStreamingResults,
    ) -> capnp::Result<()> {
        let p = params.get()?;
        let path = scoped_path(&self.prefix, p.get_path()?.to_str()?)?;
        let mut headers = crate::http_adapter::request_headers(p.get_context()?)?;
        crate::streaming::content_headers(
            &mut headers,
            p.get_mime_type()?.to_str()?,
            p.get_encoding()?.to_str()?,
        )?;
        headers.push((
            "x-sandstorm-session-type".into(),
            if self.native == "apiSession" {
                "api"
            } else {
                "web"
            }
            .into(),
        ));
        let controller = self
            .controller
            .upgrade()
            .ok_or_else(|| invalid("grain closed"))?;
        if controller.is_recovering() {
            return Err(invalid("application awaits recovery"));
        }
        let runtime = controller.snapshot().1;
        results.get().set_stream(
            crate::streaming::upload(
                &runtime,
                crate::streaming::Request {
                    method: HttpMethod::Post,
                    path,
                    headers,
                    revision: None,
                },
                p.get_expected_size(),
                p.get_context()?.get_response_stream()?,
            )
            .await?,
        );
        Ok(())
    }
    async fn put_streaming(
        self: Rc<Self>,
        params: web::PutStreamingParams,
        mut results: web::PutStreamingResults,
    ) -> capnp::Result<()> {
        let p = params.get()?;
        let path = scoped_path(&self.prefix, p.get_path()?.to_str()?)?;
        let mut headers = crate::http_adapter::request_headers(p.get_context()?)?;
        crate::streaming::content_headers(
            &mut headers,
            p.get_mime_type()?.to_str()?,
            p.get_encoding()?.to_str()?,
        )?;
        headers.push((
            "x-sandstorm-session-type".into(),
            if self.native == "apiSession" {
                "api"
            } else {
                "web"
            }
            .into(),
        ));
        let controller = self
            .controller
            .upgrade()
            .ok_or_else(|| invalid("grain closed"))?;
        if controller.is_recovering() {
            return Err(invalid("application awaits recovery"));
        }
        let runtime = controller.snapshot().1;
        results.get().set_stream(
            crate::streaming::upload(
                &runtime,
                crate::streaming::Request {
                    method: HttpMethod::Put,
                    path,
                    headers,
                    revision: None,
                },
                p.get_expected_size(),
                p.get_context()?.get_response_stream()?,
            )
            .await?,
        );
        Ok(())
    }

    async fn get(
        self: Rc<Self>,
        params: web::GetParams,
        results: web::GetResults,
    ) -> capnp::Result<()> {
        let p = params.get()?;
        let method = if p.get_ignore_body() {
            HttpMethod::Head
        } else {
            HttpMethod::Get
        };
        self.respond(
            method,
            p.get_path()?.to_str()?.to_owned(),
            vec![],
            crate::http_adapter::request_headers(p.get_context()?)?,
            crate::grain::response_stream(p.get_context()?)?,
            results,
        )
        .await
    }
    async fn post(
        self: Rc<Self>,
        params: web::PostParams,
        results: web::PostResults,
    ) -> capnp::Result<()> {
        let p = params.get()?;
        let content = p.get_content()?;
        let headers = upload_headers(
            content.get_content()?,
            content.get_mime_type()?.to_str()?,
            content.get_encoding()?.to_str()?,
            p.get_context()?,
        )?;
        self.respond(
            HttpMethod::Post,
            p.get_path()?.to_str()?.to_owned(),
            content.get_content()?.to_vec(),
            headers,
            crate::grain::response_stream(p.get_context()?)?,
            results,
        )
        .await
    }
    async fn put(
        self: Rc<Self>,
        params: web::PutParams,
        results: web::PutResults,
    ) -> capnp::Result<()> {
        let p = params.get()?;
        let content = p.get_content()?;
        let headers = upload_headers(
            content.get_content()?,
            content.get_mime_type()?.to_str()?,
            content.get_encoding()?.to_str()?,
            p.get_context()?,
        )?;
        self.respond(
            HttpMethod::Put,
            p.get_path()?.to_str()?.to_owned(),
            content.get_content()?.to_vec(),
            headers,
            crate::grain::response_stream(p.get_context()?)?,
            results,
        )
        .await
    }
    async fn patch(
        self: Rc<Self>,
        params: web::PatchParams,
        results: web::PatchResults,
    ) -> capnp::Result<()> {
        let p = params.get()?;
        let content = p.get_content()?;
        let headers = upload_headers(
            content.get_content()?,
            content.get_mime_type()?.to_str()?,
            content.get_encoding()?.to_str()?,
            p.get_context()?,
        )?;
        self.respond(
            HttpMethod::Patch,
            p.get_path()?.to_str()?.to_owned(),
            content.get_content()?.to_vec(),
            headers,
            crate::grain::response_stream(p.get_context()?)?,
            results,
        )
        .await
    }
    async fn delete(
        self: Rc<Self>,
        params: web::DeleteParams,
        results: web::DeleteResults,
    ) -> capnp::Result<()> {
        let params = params.get()?;
        let path = params.get_path()?.to_str()?.to_owned();
        let headers = crate::http_adapter::request_headers(params.get_context()?)?;
        self.respond(
            HttpMethod::Delete,
            path,
            vec![],
            headers,
            crate::grain::response_stream(params.get_context()?)?,
            results,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_stay_in_scope_and_cannot_reach_runtime_routes() {
        assert_eq!(
            scoped_path("/shared", "/info?q=hello%20world").unwrap(),
            "shared/info?q=hello%20world"
        );
        assert_eq!(scoped_path("/shared/", "").unwrap(), "shared/");
        for path in [
            "../secret",
            "%2e%2e/secret",
            "a/../../secret",
            "a\\..\\secret",
            "%252e%252e/secret",
            "%2fsecret",
            "http://host/",
            "a#fragment",
            "%0asecret",
        ] {
            assert!(scoped_path("/shared", path).is_err(), "{path}");
        }
        for path in [
            "__spki/v1/activate",
            "%5f_spki_internal_connect",
            "__sandstorm/native-capnp/rpc",
            "capnp-es/index.mjs",
        ] {
            assert!(scoped_path("", path).is_err(), "{path}");
        }
        for path in [
            "/shared/..",
            "/shared/%2e%2e",
            "shared",
            "/shared?q=x",
            "/%5f_spki",
        ] {
            assert!(
                create(Weak::new(), "webSession", path, true).is_err(),
                "{path}"
            );
        }
    }
    #[test]
    fn persistent_ids_round_trip_and_ephemeral_caps_reject_save() {
        futures::executor::block_on(async {
            for native in ["webSession", "apiSession"] {
                let cap = create(Weak::new(), native, "/shared/", true).unwrap();
                let persistent = app_persistent::Client::<capnp::any_pointer::Owned>::new(cap.hook);
                let response = persistent.save_request().send().promise.await.unwrap();
                let id = response.get().unwrap().get_object_id().unwrap();
                let parsed = id.get_as::<route_object_id::Reader>().unwrap();
                assert_eq!(parsed.get_path_prefix().unwrap(), "/shared");
                assert_eq!(parsed.get_native_interface().unwrap(), native);
                restore(Weak::new(), id).unwrap();
            }
            let cap = create(Weak::new(), "webSession", "/shared", false).unwrap();
            let persistent = app_persistent::Client::<capnp::any_pointer::Owned>::new(cap.hook);
            assert!(persistent.save_request().send().promise.await.is_err());
            let mut message = capnp::message::Builder::new_default();
            message
                .init_root::<route_object_id::Builder>()
                .set_format("unknown");
            assert!(restore(Weak::new(), message.get_root_as_reader().unwrap()).is_err());
        });
    }
}
