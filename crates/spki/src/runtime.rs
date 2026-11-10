//! Validate and copy only the files recorded in a prepared runtime bundle.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};
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
pub fn stage(root: &Path, stage: &Path) -> Result<()> {
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
            entry["sha256"].as_str() == Some(hash(&bytes).as_str()),
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
