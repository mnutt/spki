#![deny(unsafe_code)]
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    os::fd::{FromRawFd, OwnedFd},
    process::Stdio,
    time::Duration,
};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    ensure!(
        args.len() == 4 && (args[1] == "probe" || args[1] == "grain" || args[1] == "app"),
        "usage: isolate-preview (probe|grain|app) HOST WORKER.js"
    );
    let preview = args[1] == "grain";
    let grain = preview || args[1] == "app";
    // Adopt the inherited socket before Tokio can allocate descriptors or start threads.
    let supervisor = if grain {
        Some(take_supervisor_socket()?)
    } else {
        None
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run(args, preview, supervisor))
}

// This is the sole raw-descriptor ownership boundary. Keep it at process startup.
#[allow(unsafe_code)]
fn take_supervisor_socket() -> Result<std::os::unix::net::UnixStream> {
    // SAFETY: Sandstorm transfers exclusive ownership of fd 3 to this process.
    // main calls this exactly once, before starting Tokio or opening any files/sockets.
    // fcntl rejects a missing/closed fd and marks it close-on-exec before any child is spawned;
    // only after that succeeds do we create the sole Rust owner. No raw fd is used afterward.
    let socket = unsafe {
        if libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC) == -1 {
            return Err(std::io::Error::last_os_error()).context("missing supervisor fd 3");
        }
        std::os::unix::net::UnixStream::from_raw_fd(3)
    };
    socket
        .peer_addr()
        .context("supervisor fd 3 is not a connected Unix socket")?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

async fn run(
    args: Vec<std::ffi::OsString>,
    preview: bool,
    supervisor: Option<std::os::unix::net::UnixStream>,
) -> Result<()> {
    let supervisor = supervisor
        .map(tokio::net::UnixStream::from_std)
        .transpose()?;
    let host = std::fs::canonicalize(&args[2])?;
    let digest = Sha256::digest(std::fs::read(&host)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    ensure!(
        digest == isolate_preview::HOST_SHA256,
        "host binary does not match the pinned release"
    );
    let js = std::fs::read(&args[3]).context("read worker")?;
    let (parent, child) = std::os::unix::net::UnixStream::pair()?;
    parent.set_nonblocking(true)?;
    let mut command = tokio::process::Command::new(host);
    command
        .args(["--control-fd", "0"])
        .stdin(Stdio::from(OwnedFd::from(child)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Command owns and remaps the control socket to child stdin. No pre_exec hook,
    // raw-fd capture, or global change to the parent's close-on-exec flags is needed.
    let mut process = command.spawn()?;
    drop(command);
    let stream = tokio::net::UnixStream::from_std(parent)?;
    let result: Result<()> = tokio::task::LocalSet::new()
        .run_until(async {
            let logs = isolate_preview::logs::Logs::new()?;
            tokio::task::spawn_local(isolate_preview::logs::capture(
                process.stdout.take().context("missing host stdout")?,
                "stdout",
                logs.clone(),
            ));
            tokio::task::spawn_local(isolate_preview::logs::capture(
                process.stderr.take().context("missing host stderr")?,
                "stderr",
                logs.clone(),
            ));
            if let Some(supervisor) = supervisor {
                use capnp_rpc::{RpcSystem, rpc_twoparty_capnp::Side, twoparty};
                use futures::AsyncReadExt;
                use tokio_util::compat::TokioAsyncReadCompatExt;
                let runtime = tokio::time::timeout(Duration::from_secs(20), async {
                    if preview {
                        isolate_preview::controller::Controller::start(stream, &js, "/var".into())
                            .await
                    } else {
                        isolate_preview::controller::Controller::start_packaged(
                            stream,
                            &js,
                            "/var".into(),
                        )
                        .await
                    }
                })
                .await??;
                let title = if preview {
                    "Isolate Preview".to_owned()
                } else {
                    let config: serde_json::Value =
                        serde_json::from_slice(&std::fs::read("/app/config.json")?)?;
                    config["title"]
                        .as_str()
                        .context("missing app title")?
                        .to_owned()
                };
                let view: isolate_protocol::grain_capnp::main_view::Client<
                    capnp::any_pointer::Owned,
                > = capnp_rpc::new_client(isolate_preview::grain::View {
                    controller: runtime.clone(),
                    logs,
                    mode: if preview {
                        isolate_preview::grain::RuntimeMode::Preview
                    } else {
                        isolate_preview::grain::RuntimeMode::Packaged
                    },
                    title,
                });
                let (read, write) = supervisor.compat().split();
                let network =
                    twoparty::VatNetwork::new(read, write, Side::Client, Default::default());
                let mut rpc = RpcSystem::new(Box::new(network), Some(view.client));
                *runtime.bridge().api.borrow_mut() = Some(rpc.bootstrap(Side::Server));
                let keepalive = async {
                    let mut interval = tokio::time::interval(Duration::from_secs(10));
                    loop {
                        interval.tick().await;
                        runtime
                            .snapshot()
                            .1
                            .worker
                            .keep_alive_request()
                            .send()
                            .promise
                            .await?;
                    }
                    #[allow(unreachable_code)]
                    Ok::<(), capnp::Error>(())
                };
                tokio::select! { result = rpc => result?, result = keepalive => result? }
            } else {
                let (status, body) = tokio::time::timeout(
                    Duration::from_secs(20),
                    isolate_preview::probe(stream, &js),
                )
                .await??;
                ensure!(status == 200, "unexpected HTTP status {status}");
                println!("{}", String::from_utf8(body)?);
            }
            Ok(())
        })
        .await;
    let _ = process.kill().await;
    let _ = process.wait().await;
    result
}
