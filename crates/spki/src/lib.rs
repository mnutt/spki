//! Shared package writing, runtime staging, and schema evaluation for CLI tools.
#![deny(unsafe_code)]
mod archive;
include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
pub mod capnp_compiler;
pub mod ids;
pub mod runtime;
pub mod spk;
