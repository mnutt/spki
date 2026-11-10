//! Shared deployment formats and runtime identity, independent of the grain server.
#![forbid(unsafe_code)]
pub mod bindings;
pub mod protocol;
use sha2::{Digest, Sha256};
pub const HOST_SHA256: &str = "94b04f0ed5ad0f3ff862dcadd1b7edfe6cad1717587a60b7d8e6775541b538b1";
// A static keeps one embedded copy shared by the runtime and the CLI crate.
pub static SDK_MODULES_JSON: &str = include_str!("../../isolate-preview/sdk/modules.json");
pub fn revision(source: &[u8]) -> String {
    let mut hash = Sha256::new();
    for data in [
        crate::HOST_SHA256.as_bytes(),
        b"2026-06-10",
        b"spki-workspace-v1",
        crate::SDK_MODULES_JSON.as_bytes(),
        include_bytes!("../../isolate-preview/sdk/entry.js"),
        include_bytes!("../../isolate-preview/browser/client.js"),
        source,
    ] {
        hash.update((data.len() as u64).to_le_bytes());
        hash.update(data);
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
pub fn validate_id(id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "project ID must contain 1–64 letters, numbers, hyphens or underscores"
    );
    Ok(())
}
