//! Shared package writing, runtime staging, and schema evaluation for CLI tools.
#![forbid(unsafe_code)]
include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
pub mod capnp_compiler;
pub mod ids;
pub mod spk;
pub mod runtime;
