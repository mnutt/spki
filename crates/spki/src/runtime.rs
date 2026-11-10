//! Stream the pinned runtime into private package staging, checking each file's hash.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};
include!(concat!(env!("OUT_DIR"), "/embedded-runtime.rs"));
const RECEIPT_LIMIT: u64 = 1024 * 1024;
const MAGIC: &[u8; 8] = b"SPKIRUN1";

fn validate(receipt: &Value) -> Result<&serde_json::Map<String, Value>> {
    ensure!(
        receipt["formatVersion"] == 1 && receipt["target"] == "linux-x86_64",
        "unsupported runtime bundle format or target"
    );
    ensure!(
        receipt["hostSha256"] == spki_common::HOST_SHA256
            && receipt["sdkRevision"] == spki_common::revision(b""),
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
            .any(|name| name.starts_with("usr/share/licenses/isolate-host/")),
        "missing host license files"
    );
    for (name, entry) in files {
        let path = Path::new(name);
        ensure!(
            !path.is_absolute()
                && path
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_)))
                && !name.contains('\\')
                && (name == "bin/isolate-preview"
                    || name == "bin/isolate-host"
                    || name.starts_with("lib/")
                    || name.starts_with("lib64/")
                    || name.starts_with("usr/lib/")
                    || name.starts_with("usr/lib64/")
                    || name.starts_with("usr/share/licenses/")),
            "invalid runtime bundle path"
        );
        ensure!(
            entry["mode"].as_u64().is_some_and(|mode| mode <= 0o777),
            "invalid runtime file mode"
        );
    }
    Ok(files)
}
fn copy_file(
    mut source: impl Read,
    destination: &Path,
    name: &str,
    entry: &Value,
    length: u64,
) -> Result<()> {
    ensure!(length <= crate::archive::LIMIT, "runtime file too large");
    fs::create_dir_all(destination.parent().unwrap())?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut header = Vec::new();
    let mut count = 0;
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count += read as u64;
        ensure!(count <= length, "runtime file grew during packaging");
        header.extend_from_slice(&buffer[..read.min(20 - header.len())]);
        hash.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
    }
    ensure!(count == length, "truncated runtime file: {name}");
    let digest: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    ensure!(
        entry["sha256"].as_str() == Some(&digest),
        "runtime file hash mismatch: {name}"
    );
    if name == "bin/isolate-host" {
        ensure!(digest == spki_common::HOST_SHA256, "incorrect host binary");
    }
    if name.starts_with("bin/") {
        ensure!(
            header.len() == 20 && &header[..6] == b"\x7fELF\x02\x01" && header[18..20] == [62, 0],
            "runtime executable must be Linux x86_64 ELF"
        );
    }
    output.set_permissions(fs::Permissions::from_mode(
        entry["mode"].as_u64().unwrap() as u32
    ))?;
    Ok(())
}
fn finish(receipt: &Value, stage: &Path) -> Result<()> {
    for dir in ["dev", "proc", "tmp", "var", "app"] {
        fs::create_dir_all(stage.join(dir))?;
    }
    fs::write(
        stage.join("app/runtime.json"),
        serde_json::to_vec_pretty(receipt)?,
    )?;
    Ok(())
}
pub fn stage(root: &Path, stage: &Path) -> Result<()> {
    let root = fs::canonicalize(root)?;
    let mut data = Vec::new();
    File::open(root.join("runtime.json"))
        .context("runtime bundle missing")?
        .take(RECEIPT_LIMIT + 1)
        .read_to_end(&mut data)?;
    ensure!(
        data.len() as u64 <= RECEIPT_LIMIT,
        "runtime receipt too large"
    );
    let receipt: Value = serde_json::from_slice(&data)?;
    let mut total = 0;
    for (name, entry) in validate(&receipt)? {
        let path = root.join(name);
        ensure!(
            fs::canonicalize(&path)?.starts_with(&root),
            "runtime file escapes bundle"
        );
        let source = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let meta = source.metadata()?;
        ensure!(meta.is_file(), "runtime file must be regular");
        total += meta.len();
        ensure!(total <= crate::archive::LIMIT, "runtime bundle too large");
        copy_file(source, &stage.join(name), name, entry, meta.len())?;
    }
    finish(&receipt, stage)
}
pub fn stage_embedded(stage: &Path) -> Result<()> {
    ensure!(
        !EMBEDDED_RUNTIME.is_empty(),
        "this development build has no embedded runtime; supply --runtime-root or build with SPKI_RUNTIME_BUNDLE"
    );
    unpack(EMBEDDED_RUNTIME, stage)
}
fn unpack(bytes: &[u8], stage: &Path) -> Result<()> {
    let mut reader = lzma_rust2::XzReader::new_mem_limit(bytes, true, 64 * 1024);
    let mut magic = [0; 8];
    reader.read_exact(&mut magic)?;
    ensure!(&magic == MAGIC, "invalid embedded runtime format");
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    ensure!(
        length as u64 <= RECEIPT_LIMIT,
        "embedded runtime receipt too large"
    );
    let mut data = vec![0; length];
    reader.read_exact(&mut data)?;
    let receipt: Value = serde_json::from_slice(&data)?;
    ensure!(
        receipt["spkiVersion"] == env!("CARGO_PKG_VERSION"),
        "embedded runtime version differs from this CLI"
    );
    let mut total = 0u64;
    // serde_json's map is sorted: file bodies follow receipt keys in lexical order.
    for (name, entry) in validate(&receipt)? {
        let length = entry["size"]
            .as_u64()
            .context("runtime file missing size")?;
        total = total
            .checked_add(length)
            .context("runtime length overflow")?;
        ensure!(total <= crate::archive::LIMIT, "runtime bundle too large");
        copy_file(
            (&mut reader).take(length),
            &stage.join(name),
            name,
            entry,
            length,
        )?;
    }
    ensure!(reader.read(&mut [0; 1])? == 0, "trailing runtime data");
    finish(&receipt, stage)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_copy_checks_lengths_hashes_and_executable_headers() -> Result<()> {
        let root = tempfile::tempdir()?;
        let entry = serde_json::json!({"mode":420, "sha256": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"});
        copy_file(
            &b"abc"[..],
            &root.path().join("good"),
            "lib/test",
            &entry,
            3,
        )?;
        assert_eq!(fs::read(root.path().join("good"))?, b"abc");
        assert!(copy_file(&b"abd"[..], &root.path().join("bad"), "lib/test", &entry, 3).is_err());
        assert!(
            copy_file(
                &b"ab"[..],
                &root.path().join("short"),
                "lib/test",
                &entry,
                3
            )
            .is_err()
        );
        assert!(
            copy_file(
                &b"abcd"[..],
                &root.path().join("long"),
                "lib/test",
                &entry,
                3
            )
            .is_err()
        );
        assert!(
            copy_file(
                &b"abc"[..],
                &root.path().join("elf"),
                "bin/isolate-preview",
                &entry,
                3
            )
            .is_err()
        );
        // Existing files must not be overwritten, even inside a staging directory.
        assert!(
            copy_file(
                &b"abc"[..],
                &root.path().join("good"),
                "lib/test",
                &entry,
                3
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn receipts_cannot_write_outside_runtime_paths() -> Result<()> {
        let entry = serde_json::json!({"mode":420, "sha256":"unused"});
        let receipt = serde_json::json!({
            "formatVersion":1, "target":"linux-x86_64",
            "hostSha256":spki_common::HOST_SHA256,
            "sdkRevision":spki_common::revision(b""),
            "files": {"bin/isolate-host":entry, "bin/isolate-preview":entry,
                "usr/share/licenses/isolate-host/LICENSE":entry}
        });
        validate(&receipt)?;
        for name in [
            "../escape",
            "/lib/absolute",
            "lib/../../escape",
            "app/worker.js",
            "lib\\escape",
        ] {
            let mut invalid = receipt.clone();
            invalid["files"][name] = entry.clone();
            assert!(validate(&invalid).is_err(), "accepted {name}");
        }
        Ok(())
    }
    #[test]
    fn embedded_format_rejects_corruption_and_oversized_receipts() -> Result<()> {
        let stage = tempfile::tempdir()?;
        assert!(unpack(b"not xz", stage.path()).is_err());
        let mut compressed = Vec::new();
        let mut writer =
            lzma_rust2::XzWriter::new(&mut compressed, lzma_rust2::XzOptions::with_preset(3))?;
        writer.write_all(MAGIC)?;
        writer.write_all(&u32::MAX.to_le_bytes())?;
        writer.finish()?;
        assert!(unpack(&compressed, stage.path()).is_err());
        Ok(())
    }
}
