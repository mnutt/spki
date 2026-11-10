//! Exercise workspace recovery through RPC using a disposable host process.
//! No Sandstorm installation or downloaded isolate host is required.
use anyhow::{Result, ensure};
use capnp::{capability::FromClientHook, traits::HasTypeId};
use futures::AsyncReadExt;
use isolate_preview::{
    controller::Controller,
    grain::{RuntimeMode, View},
    logs::Logs,
    workspace::Workspace,
};
use isolate_protocol::{
    api_session_capnp::api_session,
    grain_capnp::{session_context, ui_view},
    http_over_capnp_capnp::http_service,
    isolate_host_capnp::{hosted_isolate, isolate_host},
    web_session_capnp::web_session as web,
};
use std::{
    fs,
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::fs::PermissionsExt,
    },
    path::{Path, PathBuf},
    process::Stdio,
    rc::Rc,
    time::Duration,
};
use tokio_util::compat::TokioAsyncReadCompatExt;

struct FakeHost;
impl isolate_host::Server for FakeHost {
    async fn get_info(
        self: Rc<Self>,
        _: isolate_host::GetInfoParams,
        mut results: isolate_host::GetInfoResults,
    ) -> capnp::Result<()> {
        results.get().set_protocol_version(1);
        results.get().set_source_format_version(1);
        Ok(())
    }
    async fn start_worker(
        self: Rc<Self>,
        _: isolate_host::StartWorkerParams,
        mut results: isolate_host::StartWorkerResults,
    ) -> capnp::Result<()> {
        results.get().set_worker(capnp_rpc::new_client(FakeWorker));
        Ok(())
    }
}
struct FakeWorker;
impl hosted_isolate::Server for FakeWorker {
    async fn keep_alive(
        self: Rc<Self>,
        _: hosted_isolate::KeepAliveParams,
        _: hosted_isolate::KeepAliveResults,
    ) -> capnp::Result<()> {
        Ok(())
    }
    async fn stop(
        self: Rc<Self>,
        _: hosted_isolate::StopParams,
        _: hosted_isolate::StopResults,
    ) -> capnp::Result<()> {
        Ok(())
    }
    async fn get_http_service(
        self: Rc<Self>,
        _: hosted_isolate::GetHttpServiceParams,
        mut results: hosted_isolate::GetHttpServiceResults,
    ) -> capnp::Result<()> {
        results.get().set_service(capnp_rpc::new_client(FakeHttp));
        Ok(())
    }
}
struct Complete;
impl http_service::server_request_context::Server for Complete {}
struct FakeHttp;
impl http_service::Server for FakeHttp {
    async fn start_request(
        self: Rc<Self>,
        params: http_service::StartRequestParams,
        mut results: http_service::StartRequestResults,
    ) -> capnp::Result<()> {
        let p = params.get()?;
        let body = if p
            .get_request()?
            .get_url()?
            .to_str()?
            .ends_with("__spki_internal_ready")
        {
            "ready".into()
        } else {
            std::process::id().to_string()
        };
        let mut response = p.get_context()?.start_response_request();
        response.get().get_response()?.set_status_code(200);
        response
            .get()
            .get_response()?
            .init_body_size()
            .set_fixed(body.len() as u64);
        let result = response.send().promise.await?;
        let mut write = result.get()?.get_body()?.write_request();
        write.get().set_bytes(body.as_bytes());
        write.send().await?;
        results.get().set_context(capnp_rpc::new_client(Complete));
        Ok(())
    }
}

// The test executable also serves as the child fixture; the wrapper selects only
// this test, and an environment flag keeps an ordinary test run a no-op.
#[test]
fn fake_host_process() -> Result<()> {
    let Some(directory) = std::env::var_os("SPKI_TEST_HOST_DIRECTORY") else {
        return Ok(());
    };
    let directory = PathBuf::from(directory);
    let mode = fs::read_to_string(directory.join("mode")).unwrap_or_default();
    fs::write(directory.join(format!("pid-{}", std::process::id())), &mode)?;
    // SAFETY: the parent transfers this socket as child stdin. This fixture is
    // the sole owner and does not use stdin through any other API.
    let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(0) };
    stream.set_nonblocking(true)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(tokio::task::LocalSet::new().run_until(async {
            if mode == "hang" {
                futures::future::pending::<()>().await;
            }
            let (read, write) = tokio::net::UnixStream::from_std(stream)?.compat().split();
            let network = capnp_rpc::twoparty::VatNetwork::new(
                read,
                write,
                capnp_rpc::rpc_twoparty_capnp::Side::Server,
                Default::default(),
            );
            let host: isolate_host::Client = capnp_rpc::new_client(FakeHost);
            capnp_rpc::RpcSystem::new(Box::new(network), Some(host.client)).await?;
            Ok(())
        }))
}

fn wrapper(directory: &Path) -> Result<PathBuf> {
    let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    let file = directory.join("host");
    fs::write(
        &file,
        format!(
            "#!/bin/sh\nexport SPKI_TEST_HOST_DIRECTORY={}\nexec {} --exact fake_host_process --nocapture\n",
            quote(directory),
            quote(&std::env::current_exe()?)
        ),
    )?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o700))?;
    Ok(file)
}
struct Context;
impl session_context::Server for Context {}
async fn session(workspace: Rc<Workspace>, api: bool) -> Result<web::Client> {
    let view: ui_view::Client = capnp_rpc::new_client_from_rc(workspace);
    let mut request = view.new_session_request();
    request.get().set_context(capnp_rpc::new_client(Context));
    request.get().set_session_type(if api {
        api_session::Client::TYPE_ID
    } else {
        web::Client::TYPE_ID
    });
    request
        .get()
        .init_user_info()
        .init_permissions(1)
        .set(0, true);
    request
        .get()
        .get_session_params()
        .init_as::<web::params::Builder>();
    Ok(web::Client::new(
        request
            .send()
            .promise
            .await?
            .get()?
            .get_session()?
            .into_client_hook(),
    ))
}
fn body(response: web::response::Reader<'_>) -> Result<String> {
    let web::response::Content(content) = response.which()? else {
        anyhow::bail!("request failed")
    };
    let web::response::content::body::Bytes(bytes) = content.get_body().which()? else {
        anyhow::bail!("expected bytes")
    };
    Ok(String::from_utf8(bytes?.to_vec())?)
}
async fn get(client: &web::Client, path: &str) -> Result<String> {
    let mut request = client.get_request();
    request.get().set_path(path);
    body(request.send().promise.await?.get()?)
}
async fn post(client: &web::Client, path: &str, data: serde_json::Value) -> Result<String> {
    let mut request = client.post_request();
    request.get().set_path(path);
    request
        .get()
        .init_content()
        .set_content(serde_json::to_string(&data)?.as_bytes());
    body(request.send().promise.await?.get()?)
}
async fn create(client: &web::Client, id: &str) -> Result<()> {
    post(
        client,
        &format!("__spki/v1/projects/{id}/create"),
        serde_json::json!({}),
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn recovers_projects_without_restarting_workspace_or_existing_sessions() -> Result<()> {
    tokio::task::LocalSet::new()
        .run_until(tokio::time::timeout(
            Duration::from_secs(30),
            recovery_scenario(),
        ))
        .await?
}

async fn recovery_scenario() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let host = wrapper(directory.path())?;
    let (parent, child) = std::os::unix::net::UnixStream::pair()?;
    parent.set_nonblocking(true)?;
    let mut root_host = tokio::process::Command::new(&host)
        .stdin(Stdio::from(OwnedFd::from(child)))
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let controller = Controller::start(
        tokio::net::UnixStream::from_std(parent)?,
        b"export default {}",
        directory.path().join("root"),
    )
    .await?;
    let root = Rc::new(View {
        controller,
        logs: Logs::new()?,
        mode: RuntimeMode::Preview,
        title: "test".into(),
    });
    let workspace = Workspace::new(root, host, directory.path().join("projects"));
    let api = session(workspace.clone(), true).await?;
    let web = session(workspace.clone(), false).await?;
    let other = session(workspace.clone(), false).await?;
    let (a, b) = tokio::join!(create(&api, "alpha"), create(&api, "alpha"));
    a?;
    b?;
    create(&api, "beta").await?;
    get(&web, "__spki/open/alpha").await?;
    get(&other, "__spki/open/beta").await?;
    assert_eq!(
        fs::read_dir(directory.path())?
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("pid-"))
            .count(),
        3,
        "concurrent creates must start only one host for alpha"
    );
    let alpha_pid = get(&web, "pid").await?;
    let beta_pid = get(&other, "pid").await?;
    let info: serde_json::Value =
        serde_json::from_str(&get(&api, "__spki/v1/projects/alpha/__spki/v1/info").await?)?;
    let activated: serde_json::Value = serde_json::from_str(&post(&api, "__spki/v1/projects/alpha/__spki/v1/activate", serde_json::json!({
            "source":"export default {fetch(){return new Response('saved deployment')}}", "expectedRevision":info["revision"]
        })).await?)?;
    let storage = directory.path().join("projects/alpha/storage/retained");
    fs::write(&storage, "saved data")?;
    ensure!(
        tokio::process::Command::new("kill")
            .args(["-KILL", &alpha_pid])
            .status()
            .await?
            .success()
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(pid) = get(&web, "pid").await
                && pid != alpha_pid
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let recovered: serde_json::Value =
        serde_json::from_str(&get(&api, "__spki/v1/projects/alpha/__spki/v1/info").await?)?;
    assert_eq!(recovered["revision"], activated["revision"]);
    assert_eq!(fs::read_to_string(&storage)?, "saved data");
    assert_eq!(get(&other, "pid").await?, beta_pid);

    // A stuck startup must not block an existing project or another new one.
    fs::write(directory.path().join("mode"), "hang")?;
    let slow_api = api.clone();
    let slow = tokio::task::spawn_local(async move { create(&slow_api, "slow").await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fs::read_dir(directory.path())
                .unwrap()
                .flatten()
                .any(|entry| {
                    entry.file_name().to_string_lossy().starts_with("pid-")
                        && fs::read_to_string(entry.path()).ok().as_deref() == Some("hang")
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    fs::write(directory.path().join("mode"), "")?;
    tokio::time::timeout(Duration::from_secs(3), async {
        assert_eq!(get(&other, "pid").await?, beta_pid);
        create(&api, "fast").await
    })
    .await??;
    slow.abort();
    let _ = slow.await;
    tokio::time::timeout(Duration::from_secs(3), create(&api, "slow")).await??;
    drop(web);
    drop(other);
    drop(api);
    drop(workspace);
    root_host.kill().await?;
    root_host.wait().await?;
    Ok(())
}
