// The fixed deployment permission is independent of uploaded project metadata.
use crate::controller::Controller;
use capnp::{capability::FromClientHook, traits::HasTypeId};
use isolate_protocol::{
    api_session_capnp::api_session,
    grain_capnp::{main_view, ui_session, ui_view},
    http_over_capnp_capnp::HttpMethod,
    web_session_capnp::web_session as web,
};
use std::rc::Rc;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeMode {
    Preview,
    Packaged,
}
impl RuntimeMode {
    pub fn is_preview(self) -> bool {
        self == Self::Preview
    }
}
pub struct View {
    pub controller: Rc<Controller>,
    pub logs: Rc<crate::logs::Logs>,
    pub mode: RuntimeMode,
    pub title: String,
}
impl main_view::Server<capnp::any_pointer::Owned> for View {}
impl ui_view::Server for View {
    async fn get_view_info(
        self: Rc<Self>,
        _: ui_view::GetViewInfoParams,
        mut results: ui_view::GetViewInfoResults,
    ) -> capnp::Result<()> {
        let mut info = results.get();
        info.reborrow()
            .init_app_title()
            .set_default_text(&self.title);
        if !self.mode.is_preview() {
            return Ok(());
        }
        let mut permission = info.reborrow().init_permissions(1).get(0);
        permission.set_name("deploy");
        permission
            .reborrow()
            .init_title()
            .set_default_text("Deploy code");
        permission.init_description().set_default_text(
            "Replace the preview's code and exercise its storage and granted capabilities.",
        );
        let mut roles = info.init_roles(2);
        for (index, title, verb, deploy) in [
            (0, "Preview", "can open the preview", false),
            (1, "Developer", "can deploy code", true),
        ] {
            let mut role = roles.reborrow().get(index);
            role.reborrow().init_title().set_default_text(title);
            role.reborrow().init_verb_phrase().set_default_text(verb);
            role.init_permissions(1).set(0, deploy);
        }
        Ok(())
    }
    async fn new_session(
        self: Rc<Self>,
        params: ui_view::NewSessionParams,
        mut results: ui_view::NewSessionResults,
    ) -> capnp::Result<()> {
        let params = params.get()?;
        let api = params.get_session_type() == api_session::Client::TYPE_ID;
        if !api && params.get_session_type() != web::Client::TYPE_ID {
            return Err(capnp::Error::unimplemented("unsupported session".into()));
        }
        let user = params.get_user_info()?;
        let permissions = user.get_permissions()?;
        let deploy = !permissions.is_empty() && permissions.get(0);
        let mut headers = vec![
            (
                "x-sandstorm-session-type".into(),
                if api { "api" } else { "web" }.into(),
            ),
            (
                "x-sandstorm-username".into(),
                user.get_display_name()?
                    .get_default_text()?
                    .to_str()?
                    .to_owned(),
            ),
            (
                "x-sandstorm-preferred-handle".into(),
                user.get_preferred_handle()?.to_str()?.to_owned(),
            ),
        ];
        headers.push((
            "x-sandstorm-user-id".into(),
            user.get_identity_id()?
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        ));
        headers.push((
            "x-sandstorm-user-picture".into(),
            user.get_picture_url()?.to_str()?.to_owned(),
        ));
        headers.push((
            "x-sandstorm-user-pronouns".into(),
            format!("{:?}", user.get_pronouns()?).to_ascii_lowercase(),
        ));
        // This authoring format currently declares no application permission bits. Preview's
        // deploy bit authorizes the control plane only; never expose it as an app permission.
        headers.push(("x-sandstorm-permissions".into(), String::new()));
        if !api {
            let browser = params
                .get_session_params()
                .get_as::<web::params::Reader>()?;
            headers.push((
                "x-sandstorm-base-path".into(),
                browser.get_base_path()?.to_str()?.to_owned(),
            ));
            headers.push((
                "user-agent".into(),
                browser.get_user_agent()?.to_str()?.to_owned(),
            ));
            let languages = browser
                .get_acceptable_languages()?
                .iter()
                .map(|v| Ok(v?.to_str()?.to_owned()))
                .collect::<capnp::Result<Vec<_>>>()?;
            if !languages.is_empty() {
                headers.push(("accept-language".into(), languages.join(", ")));
            }
        }
        let browser_session = self.controller.bridge().session(params.get_context()?)?;
        headers.push(("x-sandstorm-session-id".into(), browser_session.id.clone()));
        let session: api_session::Client = capnp_rpc::new_client(Session {
            controller: self.controller.clone(),
            logs: self.logs.clone(),
            api,
            preview: self.mode.is_preview(),
            deploy,
            headers,
            browser_session,
        });
        results
            .get()
            .set_session(ui_session::Client::new(session.into_client_hook()));
        Ok(())
    }
}
struct Session {
    browser_session: Rc<crate::bridge::Session>,
    controller: Rc<Controller>,
    logs: Rc<crate::logs::Logs>,
    api: bool,
    preview: bool,
    deploy: bool,
    headers: Vec<(String, String)>,
}
impl Drop for Session {
    fn drop(&mut self) {
        let runtime = self.controller.snapshot().1;
        let id = self.browser_session.id.clone();
        runtime.bridge.release_session(&id);
        tokio::task::spawn_local(async move {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                runtime.fetch(
                    HttpMethod::Get,
                    &format!("__spki_internal_close_session?id={id}"),
                    &[],
                ),
            )
            .await;
        });
    }
}
impl ui_session::Server for Session {}
impl api_session::Server for Session {}
impl web::Server for Session {
    async fn open_web_socket(
        self: Rc<Self>,
        params: web::OpenWebSocketParams,
        mut results: web::OpenWebSocketResults,
    ) -> capnp::Result<()> {
        let params = params.get()?;
        let path = params.get_path()?.to_str()?;
        if self.api
            || path.trim_start_matches('/').split('?').next()
                != Some("__sandstorm/native-capnp/rpc-session")
        {
            return Err(capnp::Error::unimplemented(
                "unknown WebSocket endpoint".into(),
            ));
        }
        let worker = self.controller.snapshot().1.bridge.clone();
        results.get().set_server_stream(crate::browser::connect(
            params.get_client_stream()?,
            worker,
            &self.browser_session,
        )?);
        Ok(())
    }
    async fn get(
        self: Rc<Self>,
        params: web::GetParams,
        results: web::GetResults,
    ) -> capnp::Result<()> {
        let params = params.get()?;
        let path = params.get_path()?.to_str()?.to_owned();
        if path.trim_start_matches('/').split('?').next() == Some("__spki/v1/logs") {
            if !self.preview || !self.api {
                return reply(results, 404, "text/plain", b"not found");
            }
            if !self.deploy {
                return reply(
                    results,
                    403,
                    "text/plain",
                    b"deployment permission required",
                );
            }
            let mut results = results;
            let query = path.split_once('?').map(|(_, query)| query);
            return match self.logs.serve(
                params.get_context()?.get_response_stream()?,
                query,
                results.get(),
            ) {
                Ok(()) => Ok(()),
                Err(error) => reply(results, 400, "text/plain", error.to_string().as_bytes()),
            };
        }

        let headers = crate::http_adapter::request_headers(params.get_context()?)?;
        let method = if params.get_ignore_body() {
            HttpMethod::Head
        } else {
            HttpMethod::Get
        };
        self.respond(method, path, vec![], headers, results).await
    }
    async fn post(
        self: Rc<Self>,
        params: web::PostParams,
        results: web::PostResults,
    ) -> capnp::Result<()> {
        let params = params.get()?;
        let path = params.get_path()?.to_str()?.to_owned();
        let mut headers = crate::http_adapter::request_headers(params.get_context()?)?;
        let content = params.get_content()?;
        let data = content.get_content()?;
        if data.len() > 9 * 1024 * 1024 {
            return reply(results, 413, "text/plain", b"upload too large");
        }
        if !content.get_mime_type()?.is_empty() {
            headers.push((
                "content-type".into(),
                content.get_mime_type()?.to_str()?.to_owned(),
            ));
        }
        if !content.get_encoding()?.is_empty() {
            headers.push((
                "content-encoding".into(),
                content.get_encoding()?.to_str()?.to_owned(),
            ));
        }
        self.respond(HttpMethod::Post, path, data.to_vec(), headers, results)
            .await
    }
    async fn put(
        self: Rc<Self>,
        params: web::PutParams,
        results: web::PutResults,
    ) -> capnp::Result<()> {
        let params = params.get()?;
        let path = params.get_path()?.to_str()?.to_owned();
        let mut headers = crate::http_adapter::request_headers(params.get_context()?)?;
        let content = params.get_content()?;
        let data = content.get_content()?;
        if data.len() > 9 * 1024 * 1024 {
            return reply(results, 413, "text/plain", b"upload too large");
        }
        if !content.get_mime_type()?.is_empty() {
            headers.push((
                "content-type".into(),
                content.get_mime_type()?.to_str()?.to_owned(),
            ));
        }
        if !content.get_encoding()?.is_empty() {
            headers.push((
                "content-encoding".into(),
                content.get_encoding()?.to_str()?.to_owned(),
            ));
        }
        self.respond(HttpMethod::Put, path, data.to_vec(), headers, results)
            .await
    }
    async fn patch(
        self: Rc<Self>,
        params: web::PatchParams,
        results: web::PatchResults,
    ) -> capnp::Result<()> {
        let params = params.get()?;
        let path = params.get_path()?.to_str()?.to_owned();
        let mut headers = crate::http_adapter::request_headers(params.get_context()?)?;
        let content = params.get_content()?;
        let data = content.get_content()?;
        if data.len() > 9 * 1024 * 1024 {
            return reply(results, 413, "text/plain", b"upload too large");
        }
        if !content.get_mime_type()?.is_empty() {
            headers.push((
                "content-type".into(),
                content.get_mime_type()?.to_str()?.to_owned(),
            ));
        }
        if !content.get_encoding()?.is_empty() {
            headers.push((
                "content-encoding".into(),
                content.get_encoding()?.to_str()?.to_owned(),
            ));
        }
        self.respond(HttpMethod::Patch, path, data.to_vec(), headers, results)
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
        self.respond(HttpMethod::Delete, path, vec![], headers, results)
            .await
    }
}
fn reply(mut results: web::GetResults, status: u16, mime: &str, bytes: &[u8]) -> capnp::Result<()> {
    crate::http_adapter::write_response(
        results.get(),
        &crate::transport::HttpResponse {
            status,
            headers: vec![("content-type".into(), mime.into())],
            body: bytes.to_vec(),
        },
    )
}
impl Session {
    async fn respond(
        &self,
        method: HttpMethod,
        path: String,
        body: Vec<u8>,
        mut headers: Vec<(String, String)>,
        mut results: web::GetResults,
    ) -> capnp::Result<()> {
        let normalized = path.trim_start_matches('/');
        let route = normalized.split('?').next().unwrap_or(normalized);
        if self.preview && !self.api && method == HttpMethod::Get {
            match route {
                "__spki/v1/revision" => {
                    let body = serde_json::json!({"revision":self.controller.snapshot().0});
                    return reply(
                        results,
                        200,
                        "application/json",
                        body.to_string().as_bytes(),
                    );
                }
                "__spki/v1/reload.js" => {
                    return reply(
                        results,
                        200,
                        "application/javascript; charset=utf-8",
                        crate::reload::SCRIPT,
                    );
                }
                _ => (),
            }
        }
        // Reserve before dispatch, including encoded spellings; application code never sees control routes.
        let decoded = percent_encoding::percent_decode_str(normalized).decode_utf8_lossy();
        if decoded.starts_with("__spki") || decoded.starts_with("__spki_internal") {
            if !self.preview || !self.api {
                return reply(results, 404, "text/plain", b"not found");
            }
            if !self.deploy {
                return reply(
                    results,
                    403,
                    "text/plain",
                    b"deployment permission required",
                );
            }
            if method == HttpMethod::Get
                && matches!(normalized, "__spki/v1/info" | "__spki/v1/status")
            {
                let info = crate::protocol::Info {
                    protocol_version: 1,
                    revision: self.controller.snapshot().0,
                    host_sha256: crate::HOST_SHA256.into(),
                    sdk_revision: crate::controller::revision(b""),
                    deployment: true,
                    log_stream: true,
                    recovery: self.controller.is_recovering(),
                    max_source_bytes: 8 * 1024 * 1024,
                };
                let json = serde_json::to_string(&info)
                    .map_err(|e| capnp::Error::failed(e.to_string()))?;
                return reply(results, 200, "application/json", json.as_bytes());
            }
            if method == HttpMethod::Post && normalized == "__spki/v1/activate" {
                let result: anyhow::Result<String> = async {
                    let input: crate::protocol::Activation = serde_json::from_slice(&body)?;
                    self.controller
                        .replace(input.source.as_bytes(), &input.expected_revision)
                        .await
                }
                .await;
                return match result {
                    Ok(revision) => reply(
                        results,
                        200,
                        "application/json",
                        serde_json::json!({"revision":revision})
                            .to_string()
                            .as_bytes(),
                    ),
                    Err(e) => reply(
                        results,
                        if e.is::<crate::controller::RevisionConflict>() {
                            409
                        } else {
                            400
                        },
                        "application/json",
                        serde_json::json!({"error":e.to_string()})
                            .to_string()
                            .as_bytes(),
                    ),
                };
            }
            return reply(results, 404, "text/plain", b"not found");
        }
        if self.preview && self.api {
            return reply(
                results,
                404,
                "text/plain",
                b"preview API exposes deployment control only",
            );
        }
        if method == HttpMethod::Get
            && let Some(descriptor) = crate::browser::descriptor(normalized)
        {
            return match descriptor {
                Ok(bytes) => reply(results, 200, "application/json", &bytes),
                Err(error) => reply(results, 400, "text/plain", error.to_string().as_bytes()),
            };
        }
        if let Some(module) = crate::browser::module(route) {
            if method != HttpMethod::Get && method != HttpMethod::Head {
                return reply(results, 405, "text/plain", b"method not allowed");
            }
            return reply(
                results,
                200,
                "text/javascript; charset=utf-8",
                if method == HttpMethod::Head {
                    b""
                } else {
                    module.as_bytes()
                },
            );
        }
        headers.extend(self.headers.iter().cloned());
        let (revision, runtime) = self.controller.snapshot();
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            runtime.fetch_with_headers(method, &path, &body, &headers),
        )
        .await;
        match response {
            Ok(Ok(mut response)) => {
                if self.preview
                    && response.header("content-encoding").is_empty()
                    && method != HttpMethod::Head
                {
                    let mime = response.header("content-type").to_owned();
                    response.body = crate::reload::inject(
                        response.status,
                        &mime,
                        std::mem::take(&mut response.body),
                        &revision,
                    );
                }
                if method == HttpMethod::Head {
                    response.body.clear();
                }
                crate::http_adapter::write_response(results.get(), &response)
            }
            Ok(Err(e)) => reply(results, 500, "text/plain", e.to_string().as_bytes()),
            Err(_) => reply(results, 500, "text/plain", b"worker request timed out"),
        }
    }
}
