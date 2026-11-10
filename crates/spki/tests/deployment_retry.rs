//! Exercise the real watcher against a local HTTP fixture, without Sandstorm.
use isolate_preview::{controller::revision, protocol::Info};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct Server {
    endpoint: String,
    attempts: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let stopped = stop.clone();
        let recorded = attempts.clone();
        let thread = thread::spawn(move || {
            let mut live = revision(b"initial");
            while !stopped.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => panic!("accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap().to_owned();
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':') {
                        if key.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let mut status = "200 OK";
                let mut content_type = "application/json";
                let response = match path.as_str() {
                    "/__spki/v1/info" => serde_json::to_string(&Info {
                        protocol_version: 1,
                        revision: live.clone(),
                        host_sha256: isolate_preview::HOST_SHA256.into(),
                        sdk_revision: revision(b""),
                        deployment: true,
                        log_stream: true,
                        workspace: false,
                        recovery: false,
                        max_source_bytes: 8 * 1024 * 1024,
                    })
                    .unwrap(),
                    "/__spki/v1/status" => json!({"revision": live}).to_string(),
                    "/__spki/v1/activate" => {
                        let activation: Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(activation["expectedRevision"], live);
                        let mut attempts = recorded.lock().unwrap();
                        attempts.push(activation.clone());
                        if attempts.len() == 1 {
                            status = "503 Service Unavailable";
                            json!({"error": "temporary activation failure"}).to_string()
                        } else {
                            live = revision(activation["source"].as_str().unwrap().as_bytes());
                            json!({"revision": live}).to_string()
                        }
                    }
                    "/__spki/v1/logs" => {
                        content_type = "text/event-stream";
                        String::new()
                    }
                    _ => panic!("unexpected request: {path}"),
                };
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            }
        });
        Self {
            endpoint,
            attempts,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}
struct Watcher(Child);
impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn wait_for(log: &Path, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "watcher timed out:\n{}",
            fs::read_to_string(log).unwrap()
        );
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn retries_identical_output_after_failed_activation_but_skips_confirmed_output() {
    let server = Server::start();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let config = root.join("config");
    let worker = root.join("worker.js");
    let source = "export default {fetch() {return new Response('hello')}};";
    fs::write(&worker, source).unwrap();
    let auth = Command::new(env!("CARGO_BIN_EXE_spki"))
        .current_dir(root)
        .env("XDG_CONFIG_HOME", &config)
        .args([
            "auth",
            "--allow-local-http",
            "--webkey",
            &format!("{}#test-token", server.endpoint),
        ])
        .output()
        .unwrap();
    assert!(
        auth.status.success(),
        "{}",
        String::from_utf8_lossy(&auth.stderr)
    );
    let log = root.join("watch.log");
    let output = fs::File::create(&log).unwrap();
    let mut watcher = Watcher(
        Command::new(env!("CARGO_BIN_EXE_spki"))
            .current_dir(root)
            .env("XDG_CONFIG_HOME", &config)
            .args(["dev", "--entry"])
            .arg(&worker)
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .unwrap(),
    );
    wait_for(&log, || {
        fs::read_to_string(&log)
            .unwrap()
            .contains("edit the project to retry")
    });
    // Saving unchanged source triggers a build producing exactly the failed bundle.
    fs::write(&worker, source).unwrap();
    wait_for(&log, || {
        fs::read_to_string(&log).unwrap().contains("Live revision")
    });
    {
        let attempts = server.attempts.lock().unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(
            attempts[0], attempts[1],
            "retry must use the same source and expected revision"
        );
    }
    fs::write(&worker, source).unwrap();
    thread::sleep(Duration::from_secs(2));
    assert!(watcher.0.try_wait().unwrap().is_none(), "watcher exited");
    assert_eq!(
        server.attempts.lock().unwrap().len(),
        2,
        "confirmed output must not deploy again"
    );
}
