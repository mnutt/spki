use crate::{compiler::Compiler, ids, package_capnp as p, project, spk};
use anyhow::{Context, Result, ensure};
use capnp::{message::Builder, serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn path_ok(path: &Path) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}
fn stage_runtime(root: &Path, stage: &Path) -> Result<()> {
    let receipt: Value = serde_json::from_slice(&fs::read(root.join("runtime.json")).context(
        "runtime bundle missing; run scripts/stage-preview.py or supply a prepared --runtime-root",
    )?)?;
    ensure!(
        receipt["formatVersion"] == 1 && receipt["target"] == "linux-x86_64",
        "unsupported runtime bundle format or target"
    );
    ensure!(
        receipt["hostSha256"] == isolate_preview::HOST_SHA256
            && receipt["sdkRevision"] == isolate_preview::controller::revision(b""),
        "runtime bundle differs from this CLI"
    );
    let files = receipt["files"]
        .as_object()
        .context("runtime receipt lacks file list")?;
    for required in ["bin/isolate-preview", "bin/isolate-host"] {
        ensure!(
            files.contains_key(required),
            "missing runtime executable {required}"
        );
    }
    ensure!(
        files
            .keys()
            .any(|n| n.starts_with("usr/share/licenses/isolate-host/")),
        "missing host license files"
    );
    let root = fs::canonicalize(root)?;
    for (name, entry) in files {
        let path = Path::new(name);
        ensure!(
            path_ok(path)
                && (name == "bin/isolate-preview"
                    || name == "bin/isolate-host"
                    || name.starts_with("lib/")
                    || name.starts_with("lib64/")
                    || name.starts_with("usr/lib/")
                    || name.starts_with("usr/lib64/")
                    || name.starts_with("usr/share/licenses/")),
            "invalid runtime bundle path"
        );
        let source = root.join(path);
        let meta = fs::symlink_metadata(&source)?;
        ensure!(
            meta.is_file() && fs::canonicalize(&source)?.starts_with(&root),
            "runtime file escapes bundle or is not a regular file"
        );
        let bytes = fs::read(&source)?;
        ensure!(
            entry["sha256"].as_str() == Some(&hash(&bytes)),
            "runtime file hash mismatch: {name}"
        );
        if name == "bin/isolate-host" {
            ensure!(
                hash(&bytes) == isolate_preview::HOST_SHA256,
                "incorrect host binary"
            );
        }
        if name.starts_with("bin/") {
            ensure!(
                bytes.len() > 20 && &bytes[..6] == b"\x7fELF\x02\x01" && bytes[18..20] == [62, 0],
                "runtime executable must be Linux x86_64 ELF"
            );
        }
        let mode = entry["mode"]
            .as_u64()
            .context("runtime file missing mode")?;
        ensure!(mode <= 0o777, "invalid runtime file mode");
        let destination = stage.join(path);
        fs::create_dir_all(destination.parent().unwrap())?;
        fs::write(&destination, bytes)?;
        fs::set_permissions(destination, fs::Permissions::from_mode(mode as u32))?;
    }
    for dir in ["dev", "proc", "tmp", "var", "app"] {
        fs::create_dir_all(stage.join(dir))?;
    }
    fs::write(
        stage.join("app/runtime.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    Ok(())
}
fn command(mut command: p::manifest::command::Builder<'_>) {
    let mut argv = command.reborrow().init_argv(4);
    for (i, arg) in [
        "/bin/isolate-preview",
        "app",
        "/bin/isolate-host",
        "/app/worker.js",
    ]
    .iter()
    .enumerate()
    {
        argv.set(i as u32, *arg);
    }
}
pub struct Options {
    pub output: PathBuf,
    pub runtime_root: PathBuf,
    pub keyring: PathBuf,
    pub app_id: Option<String>,
    pub esbuild: Option<PathBuf>,
    pub target: String,
}
pub fn pack(file: &Path, options: Options) -> Result<()> {
    ensure!(
        options.target == "linux-x86_64",
        "only linux-x86_64 packages are supported"
    );
    ensure!(
        !options.output.exists(),
        "output already exists; choose a fresh release filename"
    );
    let settings = project::resolve(Some(file), None, options.esbuild.as_deref(), None)?;
    let (metadata, bytes) = project::package_metadata(file)?;
    let configured = metadata["id"].as_str().filter(|id| !id.is_empty());
    if let (Some(configured), Some(requested)) = (configured, options.app_id.as_deref()) {
        ensure!(configured == requested, "--app-id conflicts with pkgdef.id");
    }
    let id = options.app_id.as_deref().or(configured).context(
        "set pkgdef.id or --app-id; create a new signing identity explicitly with spki keygen",
    )?;
    let key = spk::identity(&options.keyring, id)?;
    let code =
        Compiler::new(settings.compiler.clone())?.build_project(&settings, 8 * 1024 * 1024)?;
    let mut input = bytes.as_slice();
    let reader = serialize::read_message_from_flat_slice(
        &mut input,
        spk::limits(p::manifest::SIZE_LIMIT_IN_WORDS as usize),
    )?;
    ensure!(input.is_empty(), "trailing manifest data");
    let mut manifest = Builder::new_default();
    manifest.set_root(reader.get_root::<p::manifest::Reader>()?)?;
    let title;
    {
        let mut m = manifest.get_root::<p::manifest::Builder>()?;
        title = m
            .reborrow()
            .get_app_title()?
            .get_default_text()?
            .to_str()?
            .to_owned();
        let actions = m.reborrow().get_actions()?;
        ensure!(
            actions.len() == 1,
            "pack currently supports exactly one creation action"
        );
        let mut action = actions.get(0);
        ensure!(
            matches!(
                action.reborrow().get_input().which()?,
                p::manifest::action::input::None(())
            ),
            "pack currently supports only a no-input creation action"
        );
        ensure!(!action.has_command(), "action command is reserved for spki");
        command(action.init_command());
        ensure!(
            !m.has_continue_command(),
            "continue command is reserved for spki"
        );
        command(m.init_continue_command());
    }
    let stage = tempfile::tempdir()?;
    stage_runtime(&options.runtime_root, stage.path())?;
    fs::write(stage.path().join("app/worker.js"), code.as_bytes())?;
    fs::write(
        stage.path().join("app/config.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"title":title,"revision":isolate_preview::controller::revision(code.as_bytes())}),
        )?,
    )?;
    fs::write(
        stage.path().join("sandstorm-manifest"),
        serialize::write_message_to_words(&manifest),
    )?;
    println!(
        "Signing {} for app {}",
        options.output.display(),
        ids::app_id(key.verifying_key().as_bytes())
    );
    spk::write(stage.path(), &key, &options.output)?;
    println!("Verified {}", options.output.display());
    Ok(())
}
