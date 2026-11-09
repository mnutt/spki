//! Revision selection is atomic; worker initialization side effects are not rolled back.
use crate::{Host, Runtime};
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    io::Write,
    path::PathBuf,
    rc::Rc,
};

pub struct Controller {
    host: Rc<Host>,
    root: PathBuf,
    current: RefCell<(String, Rc<Runtime>)>,
    recovery: Cell<bool>,
    commit: tokio::sync::Mutex<()>,
}
struct Candidate(Option<Rc<Runtime>>);
impl Drop for Candidate {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            tokio::task::spawn_local(async move {
                let _ = runtime.worker.stop_request().send().promise.await;
            });
        }
    }
}
pub fn revision(source: &[u8]) -> String {
    let mut hash = Sha256::new();
    for data in [
        crate::HOST_SHA256.as_bytes(),
        b"2026-06-10",
        include_bytes!("../sdk/modules.json"),
        include_bytes!("../sdk/entry.js"),
        include_bytes!("../browser/client.js"),
        source,
    ] {
        hash.update((data.len() as u64).to_le_bytes());
        hash.update(data);
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
impl Controller {
    pub async fn start(
        stream: tokio::net::UnixStream,
        initial: &[u8],
        root: PathBuf,
    ) -> Result<Rc<Self>> {
        Self::start_mode(stream, initial, root, true).await
    }
    pub async fn start_packaged(
        stream: tokio::net::UnixStream,
        initial: &[u8],
        root: PathBuf,
    ) -> Result<Rc<Self>> {
        Self::start_mode(stream, initial, root, false).await
    }
    async fn start_mode(
        stream: tokio::net::UnixStream,
        initial: &[u8],
        root: PathBuf,
        preview: bool,
    ) -> Result<Rc<Self>> {
        let read_root = root.clone();
        let saved = crate::disk::run(move || {
            std::fs::create_dir_all(&read_root)?;
            if preview {
                read_saved(&read_root)
            } else {
                Ok(None)
            }
        })
        .await?;
        let recovery = saved
            .as_ref()
            .is_some_and(|record| record.revision != revision(record.source.as_bytes()));
        let source = if recovery {
            RECOVERY_SOURCE.to_vec()
        } else {
            saved.map_or_else(|| initial.to_vec(), |record| record.source.into_bytes())
        };
        let host = Host::connect(stream).await?;
        let runtime = Rc::new(host.load(&source, root.join("storage")).await?);
        let mut guard = Candidate(Some(runtime.clone()));
        runtime.validate().await?;
        let controller = Rc::new(Self {
            host,
            root,
            current: RefCell::new((revision(&source), runtime)),
            recovery: Cell::new(recovery),
            commit: tokio::sync::Mutex::new(()),
        });
        if preview && !recovery {
            controller.persist(&source).await?;
        }
        guard.0 = None;
        Ok(controller)
    }
    pub fn bridge(&self) -> Rc<crate::bridge::State> {
        self.host.bridge.clone()
    }
    pub fn snapshot(&self) -> (String, Rc<Runtime>) {
        self.current.borrow().clone()
    }
    pub fn is_recovering(&self) -> bool {
        self.recovery.get()
    }
    async fn persist(&self, source: &[u8]) -> Result<()> {
        let record = SavedRevision {
            revision: revision(source),
            source: std::str::from_utf8(source)?.to_owned(),
        };
        let root = self.root.clone();
        let recovery = self.recovery.get();
        crate::disk::run(move || {
            if recovery {
                // Keep the original source recoverable even after the first successful redeploy.
                let bytes = std::fs::read(root.join("active.json"))?;
                write_record(&root, "active.previous.json", &bytes)?;
            }
            write_record(&root, "active.json", &serde_json::to_vec(&record)?)
        })
        .await
    }
    pub async fn replace(self: &Rc<Self>, source: &[u8], expected: &str) -> Result<String> {
        // An RPC cancellation must not interrupt a disk commit after rename but before the
        // in-memory swap. A dropped acknowledgement can be reconciled through status.
        let controller = self.clone();
        let source = source.to_vec();
        let expected = expected.to_owned();
        tokio::task::spawn_local(async move { controller.replace_inner(&source, &expected).await })
            .await?
    }
    async fn replace_inner(&self, source: &[u8], expected: &str) -> Result<String> {
        let revision = revision(source);
        let (current, _) = self.snapshot();
        // Retrying an already successful activation is idempotent.
        if current == revision && !self.recovery.get() {
            return Ok(revision);
        }
        ensure!(current == expected, RevisionConflict);
        let runtime = Rc::new(self.host.load(source, self.root.join("storage")).await?);
        let mut guard = Candidate(Some(runtime.clone()));
        runtime.validate().await?;
        // Serialize commits across asynchronous disk I/O, then recheck the optimistic revision.
        let _commit = self.commit.lock().await;
        ensure!(self.snapshot().0 == expected, RevisionConflict);
        self.persist(source).await?;
        let (_, old) = self.current.replace((revision.clone(), runtime));
        self.recovery.set(false);
        guard.0 = None;
        tokio::task::spawn_local(async move {
            // Existing requests retain their snapshot until completion or their request timeout.
            while Rc::strong_count(&old) > 1 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            let _ = old.worker.stop_request().send().promise.await;
        });
        Ok(revision)
    }
}

#[derive(Debug)]
pub struct RevisionConflict;
impl std::fmt::Display for RevisionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("revision conflict")
    }
}
impl std::error::Error for RevisionConflict {}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedRevision {
    revision: String,
    source: String,
}
fn read_saved(root: &std::path::Path) -> Result<Option<SavedRevision>> {
    use std::io::Read;
    match std::fs::File::open(root.join("active.json")) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(64 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 64 * 1024 * 1024, "stored revision too large");
            let record: SavedRevision = serde_json::from_slice(&bytes)?;
            ensure!(
                record.source.len() <= 8 * 1024 * 1024,
                "stored source too large"
            );
            ensure!(
                record.revision.len() == 64
                    && record.revision.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid saved revision"
            );
            Ok(Some(record))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn write_record(root: &std::path::Path, name: &str, bytes: &[u8]) -> Result<()> {
    let mut temp = tempfile::NamedTempFile::new_in(root)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(root.join(name))?;
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}
const RECOVERY_SOURCE: &[u8] = br#"export default { fetch() { return new Response('This preview needs redeployment after a runtime or SDK upgrade. Run spki dev with the matching CLI. Your previous source and storage have been preserved.', {headers:{'content-type':'text/plain; charset=utf-8'}}); } };"#;
