//! JSON messages shared by the Preview control plane and CLI.
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Activation {
    pub source: String,
    pub expected_revision: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub protocol_version: u16,
    pub revision: String,
    pub host_sha256: String,
    pub sdk_revision: String,
    pub deployment: bool,
    #[serde(default)]
    pub log_stream: bool,
    #[serde(default)]
    pub recovery: bool,
    pub max_source_bytes: usize,
}
#[derive(Serialize, Deserialize)]
pub struct Revision {
    pub revision: String,
}
#[derive(Serialize, Deserialize)]
pub struct Error {
    pub error: String,
}
