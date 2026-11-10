//! Preview projects share a grain, but own their host process, storage, logs and RPC state.
use crate::{
    controller::Controller,
    grain::{RuntimeMode, View, reply},
    logs::Logs,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use capnp::capability::FromClientHook;
use capnp::traits::HasTypeId;
use isolate_protocol::{
    api_session_capnp::api_session,
    grain_capnp::{main_view, ui_session, ui_view},
    web_session_capnp::web_session as web,
};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    os::fd::OwnedFd,
    path::PathBuf,
    process::Stdio,
    rc::{Rc, Weak},
    time::Duration,
};

const EMPTY: &[u8] =
    b"export default {fetch(){return new Response('Waiting for the first deployment.')}};";
const MAX_PROJECTS: usize = 16;
fn error(value: impl ToString) -> capnp::Error {
    capnp::Error::failed(value.to_string())
}
pub use spki_common::validate_id;
struct ProjectRuntime {
    view: Rc<View>,
    alive: Rc<Cell<bool>>,
    monitor: tokio::task::JoinHandle<()>,
}
impl Drop for ProjectRuntime {
    fn drop(&mut self) {
        self.monitor.abort();
    }
}
// Each project serializes its own startup; other projects can continue serving.
#[derive(Default)]
struct ProjectSlot {
    runtime: tokio::sync::Mutex<Option<ProjectRuntime>>,
}
pub struct Workspace {
    pub root: Rc<View>,
    host: PathBuf,
    directory: PathBuf,
    projects: RefCell<BTreeMap<String, Rc<ProjectSlot>>>,
    admission: tokio::sync::Mutex<()>,
}
impl Workspace {
    pub fn new(root: Rc<View>, host: PathBuf, directory: PathBuf) -> Rc<Self> {
        Rc::new(Self {
            root,
            host,
            directory,
            projects: RefCell::new(BTreeMap::new()),
            admission: tokio::sync::Mutex::new(()),
        })
    }
    async fn names(&self) -> anyhow::Result<Vec<String>> {
        let directory = self.directory.clone();
        crate::disk::run(move || {
            let entries = match std::fs::read_dir(directory) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
                Err(e) => return Err(e.into()),
            };
            let mut names = Vec::new();
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if entry.file_type()?.is_dir()
                    && validate_id(&name).is_ok()
                    && entry.path().join("active.json").is_file()
                {
                    names.push(name);
                }
            }
            names.sort();
            Ok(names)
        })
        .await
    }
    async fn project(&self, id: &str, create: bool) -> anyhow::Result<Rc<View>> {
        if id.is_empty() {
            return Ok(self.root.clone());
        }
        validate_id(id)?;
        let slot = self.slot(id, create).await?;
        let mut current = slot.runtime.lock().await;
        if let Some(runtime) = current.as_ref().filter(|runtime| runtime.alive.get()) {
            return Ok(runtime.view.clone());
        }
        // Drop a failed generation before spawning its replacement. Startup cancellation
        // leaves an empty slot, so the next request can retry from persisted state.
        *current = None;
        let runtime = self.start_project(id).await?;
        let view = runtime.view.clone();
        *current = Some(runtime);
        Ok(view)
    }
    async fn slot(&self, id: &str, create: bool) -> anyhow::Result<Rc<ProjectSlot>> {
        if let Some(slot) = self.projects.borrow().get(id) {
            return Ok(slot.clone());
        }
        // Only admission and the on-disk project count are global. Never hold this
        // lock while starting or contacting a host.
        let _admission = self.admission.lock().await;
        if let Some(slot) = self.projects.borrow().get(id) {
            return Ok(slot.clone());
        }
        let names = self.names().await?;
        anyhow::ensure!(
            names.iter().any(|name| name == id) || create,
            "unknown workspace project"
        );
        let mut projects = self.projects.borrow_mut();
        // Failed/cancelled starts must not consume the project limit forever.
        projects.retain(|_, slot| {
            Rc::strong_count(slot) > 1
                || slot
                    .runtime
                    .try_lock()
                    .map_or(true, |runtime| runtime.is_some())
        });
        let reserved: BTreeSet<_> = names
            .iter()
            .chain(projects.keys())
            .map(String::as_str)
            .collect();
        anyhow::ensure!(
            reserved.contains(id) || reserved.len() < MAX_PROJECTS,
            "workspace project limit reached (16)"
        );
        let slot = Rc::new(ProjectSlot::default());
        projects.insert(id.into(), slot.clone());
        Ok(slot)
    }
    async fn start_project(&self, id: &str) -> anyhow::Result<ProjectRuntime> {
        let (parent, child) = std::os::unix::net::UnixStream::pair()?;
        parent.set_nonblocking(true)?;
        let mut process = tokio::process::Command::new(&self.host)
            .args(["--control-fd", "0"])
            .stdin(Stdio::from(OwnedFd::from(child)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let logs = Logs::new()?;
        tokio::task::spawn_local(crate::logs::capture(
            process.stdout.take().unwrap(),
            "stdout",
            logs.clone(),
        ));
        tokio::task::spawn_local(crate::logs::capture(
            process.stderr.take().unwrap(),
            "stderr",
            logs.clone(),
        ));
        let controller = tokio::time::timeout(
            Duration::from_secs(20),
            Controller::start_project(
                tokio::net::UnixStream::from_std(parent)?,
                EMPTY,
                self.directory.join(id),
                id.into(),
            ),
        )
        .await??;
        *controller.bridge().api.borrow_mut() = self.root.controller.bridge().api.borrow().clone();
        let view = Rc::new(View {
            controller: controller.clone(),
            logs,
            mode: RuntimeMode::Preview,
            title: id.into(),
        });
        let alive = Rc::new(Cell::new(true));
        let live = alive.clone();
        let monitor = tokio::task::spawn_local(async move {
            // Mark the generation dead even if this task is cancelled.
            struct Liveness(Rc<Cell<bool>>);
            impl Drop for Liveness {
                fn drop(&mut self) {
                    self.0.set(false);
                }
            }
            let _liveness = Liveness(live.clone());
            let keepalive = async {
                let mut interval = tokio::time::interval(Duration::from_secs(10));
                loop {
                    interval.tick().await;
                    let result = tokio::time::timeout(
                        Duration::from_secs(20),
                        controller
                            .snapshot()
                            .1
                            .worker
                            .keep_alive_request()
                            .send()
                            .promise,
                    )
                    .await;
                    if !matches!(result, Ok(Ok(_))) {
                        break;
                    }
                }
            };
            tokio::select! {
                _ = process.wait() => (),
                _ = keepalive => (),
            }
            live.set(false);
            controller.snapshot().1.bridge.stop();
            // A broken RPC connection can leave a live but unusable child behind.
            let _ = process.kill().await;
            let _ = process.wait().await;
        });
        Ok(ProjectRuntime {
            view,
            alive,
            monitor,
        })
    }
    async fn page(
        &self,
        selected: Option<&str>,
        app_path: &str,
        results: web::GetResults,
    ) -> capnp::Result<()> {
        let names = self.names().await.map_err(error)?;
        let mut options = String::from("<option value=\"/\" disabled");
        if selected.is_none() {
            options.push_str(" selected");
        }
        options.push_str(">Choose a project</option>");
        for id in names.iter().map(String::as_str).chain(std::iter::once("")) {
            let label = if id.is_empty() { "Legacy preview" } else { id };
            options.push_str(&format!(
                "<option value=\"{id}\"{}>{label}</option>",
                if selected == Some(id) {
                    " selected"
                } else {
                    ""
                }
            ));
        }
        let body = if selected.is_some() {
            let path = html(app_path);
            format!(
                "<iframe id=\"project-preview\" title=\"Application preview\" data-initial-path=\"{path}\"></iframe>"
            )
        } else {
            let mut body = String::from(
                "<main><h1>Your projects</h1><p>Run <code>spki dev</code> to add a project to this workspace.</p><ul>",
            );
            for name in names {
                body.push_str(&format!(
                    "<li><a href=\"/__spki/open/{name}\">{name}</a></li>"
                ));
            }
            body.push_str("</ul><p><a href=\"/__spki/open/\">Open legacy preview</a></p></main>");
            body
        };
        let page = include_str!("../browser/workspace.html")
            .replace("<!--OPTIONS-->", &options)
            .replace("<!--PREVIEW-->", &body);
        reply(results, 200, "text/html; charset=utf-8", page.as_bytes())
    }
}
impl main_view::Server<capnp::any_pointer::Owned> for Workspace {
    async fn restore(
        self: Rc<Self>,
        params: main_view::RestoreParams<capnp::any_pointer::Owned>,
        mut results: main_view::RestoreResults<capnp::any_pointer::Owned>,
    ) -> capnp::Result<()> {
        let id = params.get()?.get_object_id()?;
        let project = id
            .get_as::<isolate_protocol::route_session_capnp::route_object_id::Reader>()?
            .get_project_id()?
            .to_str()?;
        let view = self.project(project, false).await.map_err(error)?;
        let cap = crate::routes::restore(Rc::downgrade(&view.controller), id)?;
        results.get().get_cap().set_as_capability(cap.hook);
        Ok(())
    }
    async fn drop(
        self: Rc<Self>,
        params: main_view::DropParams<capnp::any_pointer::Owned>,
        _: main_view::DropResults<capnp::any_pointer::Owned>,
    ) -> capnp::Result<()> {
        let id = params.get()?.get_object_id()?;
        let project = id
            .get_as::<isolate_protocol::route_session_capnp::route_object_id::Reader>()?
            .get_project_id()?
            .to_str()?;
        let view = self.project(project, false).await.map_err(error)?;
        crate::routes::restore(Rc::downgrade(&view.controller), id)?;
        Ok(())
    }
}
impl ui_view::Server for Workspace {
    async fn get_view_info(
        self: Rc<Self>,
        params: ui_view::GetViewInfoParams,
        results: ui_view::GetViewInfoResults,
    ) -> capnp::Result<()> {
        ui_view::Server::get_view_info(self.root.clone(), params, results).await
    }
    async fn new_session(
        self: Rc<Self>,
        params: ui_view::NewSessionParams,
        mut results: ui_view::NewSessionResults,
    ) -> capnp::Result<()> {
        let p = params.get()?;
        let api = p.get_session_type() == api_session::Client::TYPE_ID;
        if !api && p.get_session_type() != web::Client::TYPE_ID {
            return Err(error("unsupported session"));
        }
        let permissions = p.get_user_info()?.get_permissions()?;
        let deploy = !permissions.is_empty() && permissions.get(0);
        let session: api_session::Client = capnp_rpc::new_client(Session {
            workspace: self,
            params,
            api,
            deploy,
            selected: RefCell::new(None),
            sessions: RefCell::new(BTreeMap::new()),
        });
        results
            .get()
            .set_session(ui_session::Client::new(session.into_client_hook()));
        Ok(())
    }
}
struct Session {
    workspace: Rc<Workspace>,
    params: ui_view::NewSessionParams,
    api: bool,
    deploy: bool,
    selected: RefCell<Option<String>>,
    sessions: RefCell<BTreeMap<String, (Weak<View>, web::Client)>>,
}
impl Session {
    async fn client(&self, id: &str) -> capnp::Result<web::Client> {
        let view = self.workspace.project(id, false).await.map_err(error)?;
        if let Some((generation, session)) = self.sessions.borrow().get(id)
            && generation
                .upgrade()
                .is_some_and(|old| Rc::ptr_eq(&old, &view))
        {
            return Ok(session.clone());
        }
        // Rebuild the app session after recovery, even when this workspace session
        // is still open. Do not replay the failed request itself.
        self.sessions.borrow_mut().remove(id);
        let generation = Rc::downgrade(&view);
        let view: ui_view::Client = capnp_rpc::new_client_from_rc(view);
        let mut request = view.new_session_request();
        request.set(self.params.get()?)?;
        let session = request.send().promise.await?.get()?.get_session()?;
        let session = web::Client::new(session.into_client_hook());
        self.sessions
            .borrow_mut()
            .insert(id.into(), (generation, session.clone()));
        Ok(session)
    }
    async fn target(&self, path: &str) -> capnp::Result<(web::Client, String)> {
        crate::routes::validate_path(path)?;
        let path = path.trim_start_matches('/');
        if let Some(rest) = path.strip_prefix("__spki/v1/projects/") {
            if !self.api || !self.deploy {
                return Err(error("workspace deployment permission required"));
            }
            let (id, path) = rest
                .split_once('/')
                .ok_or_else(|| error("missing project route"))?;
            validate_id(id).map_err(error)?;
            return Ok((self.client(id).await?, path.into()));
        }
        let id = self.selected.borrow().clone().unwrap_or_default();
        Ok((self.client(&id).await?, path.into()))
    }
}
impl ui_session::Server for Session {}
impl api_session::Server for Session {}
// Forwarding keeps the existing HTTP/RPC implementation identical for legacy and workspace projects.
macro_rules! forward {
    ($method:ident, $params:ident, $results:ident, $request:ident) => {
        async fn $method(
            self: Rc<Self>,
            params: web::$params,
            mut results: web::$results,
        ) -> capnp::Result<()> {
            let p = params.get()?;
            let (client, path) = self.target(p.get_path()?.to_str()?).await?;
            let mut request = client.$request();
            request.set(p)?;
            request.get().set_path(path);
            results.set(request.send().promise.await?.get()?)
        }
    };
}
impl web::Server for Session {
    async fn get(
        self: Rc<Self>,
        params: web::GetParams,
        mut results: web::GetResults,
    ) -> capnp::Result<()> {
        let p = params.get()?;
        let path = p.get_path()?.to_str()?.trim_start_matches('/');
        if !self.api && path == "__spki/workspace.js" {
            return reply(
                results,
                200,
                "application/javascript; charset=utf-8",
                include_bytes!("../browser/workspace.js"),
            );
        }
        if !self.api && path == "__spki/workspace" {
            return self.workspace.page(None, "/", results).await;
        }
        if self.api && path == "__spki/v1/projects" {
            if !self.deploy {
                return reply(
                    results,
                    403,
                    "text/plain",
                    b"deployment permission required",
                );
            }
            let names = self.workspace.names().await.map_err(error)?;
            return reply(
                results,
                200,
                "application/json",
                serde_json::to_string(&names).map_err(error)?.as_bytes(),
            );
        }
        if !self.api
            && let Some(id) = path.strip_prefix("__spki/open/")
        {
            let (id, query) = id.split_once('?').unwrap_or((id, ""));
            if !id.is_empty() {
                validate_id(id).map_err(error)?;
            }
            let app_path = preview_path(query)?;
            let client = self.client(id).await?;
            // Switching releases the previous app session, its browser handoffs and live sockets.
            self.sessions.borrow_mut().retain(|key, _| key == id);
            *self.selected.borrow_mut() = Some(id.into());
            drop(client);
            return self.workspace.page(Some(id), &app_path, results).await;
        }
        if !self.api
            && path.is_empty()
            && self.selected.borrow().is_none()
            && !self.workspace.names().await.map_err(error)?.is_empty()
        {
            return self.workspace.page(None, "/", results).await;
        }
        let (client, path) = self.target(path).await?;
        let mut request = client.get_request();
        request.set(p)?;
        request.get().set_path(path);
        results.set(request.send().promise.await?.get()?)
    }
    async fn post(
        self: Rc<Self>,
        params: web::PostParams,
        mut results: web::PostResults,
    ) -> capnp::Result<()> {
        let p = params.get()?;
        let path = p.get_path()?.to_str()?.trim_start_matches('/');
        if let Some(id) = path
            .strip_prefix("__spki/v1/projects/")
            .and_then(|p| p.strip_suffix("/create"))
        {
            if !self.api || !self.deploy {
                return reply(
                    results,
                    403,
                    "text/plain",
                    b"deployment permission required",
                );
            }
            let result = self.workspace.project(id, true).await;
            return match result {
                Ok(_) => reply(results, 200, "application/json", b"{}"),
                Err(e) => reply(
                    results,
                    400,
                    "application/json",
                    serde_json::json!({"error":e.to_string()})
                        .to_string()
                        .as_bytes(),
                ),
            };
        }
        let (client, path) = self.target(path).await?;
        let mut request = client.post_request();
        request.set(p)?;
        request.get().set_path(path);
        results.set(request.send().promise.await?.get()?)
    }
    forward!(put, PutParams, PutResults, put_request);
    forward!(patch, PatchParams, PatchResults, patch_request);
    forward!(delete, DeleteParams, DeleteResults, delete_request);
    forward!(
        open_web_socket,
        OpenWebSocketParams,
        OpenWebSocketResults,
        open_web_socket_request
    );
    async fn post_streaming(
        self: Rc<Self>,
        params: web::PostStreamingParams,
        mut results: web::PostStreamingResults,
    ) -> capnp::Result<()> {
        if self.api {
            return Err(capnp::Error::unimplemented(
                "workspace control requests use bounded uploads".into(),
            ));
        }
        let p = params.get()?;
        let (client, path) = self.target(p.get_path()?.to_str()?).await?;
        let mut request = client.post_streaming_request();
        request.set(p)?;
        request.get().set_path(path);
        results.set(request.send().promise.await?.get()?)
    }
    forward!(
        put_streaming,
        PutStreamingParams,
        PutStreamingResults,
        put_streaming_request
    );
}

// Only local application paths may become iframe URLs; escape them at the HTML boundary.
fn html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn preview_path(query: &str) -> capnp::Result<String> {
    if query.is_empty() {
        return Ok("/".into());
    }
    let encoded = query
        .strip_prefix("path=")
        .ok_or_else(|| error("unknown preview parameter"))?;
    // Sandstorm decodes the initial grain URL; base64 preserves nested URL delimiters.
    if encoded.len() > 11000 {
        return Err(error("preview path is too long"));
    }
    let path = String::from_utf8(URL_SAFE_NO_PAD.decode(encoded).map_err(error)?).map_err(error)?;
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.len() > 8192
        || path.chars().any(|c| c.is_control())
    {
        return Err(error("preview path must be a local application URL"));
    }
    let http_path = path.split('#').next().unwrap();
    crate::routes::validate_path(http_path)?;
    let decoded = percent_encoding::percent_decode_str(http_path)
        .decode_utf8()
        .map_err(error)?;
    if decoded.starts_with("/__spki") {
        return Err(error("cannot embed workspace routes"));
    }
    Ok(path)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn iframe_paths_remain_local_and_escape_html() {
        for path in [
            "https://evil.invalid",
            "//evil.invalid",
            "/\\evil",
            "/%2e%2e/x",
            "/__spki/workspace",
            "/%5f_spki/workspace",
            "/\nx",
        ] {
            let query = format!("path={}", URL_SAFE_NO_PAD.encode(path));
            assert!(preview_path(&query).is_err(), "{path}");
        }
        assert!(preview_path("unknown=x").is_err());
        assert!(preview_path("path=invalid!").is_err());
        let path = "/counter?name=\"&value=2#tab";
        assert_eq!(
            preview_path(&format!("path={}", URL_SAFE_NO_PAD.encode(path))).unwrap(),
            path
        );
        assert_eq!(html("/x?q=\"<>&"), "/x?q=&quot;&lt;&gt;&amp;");
    }
}
