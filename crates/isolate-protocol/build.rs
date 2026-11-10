fn main() {
    println!("cargo:rerun-if-changed=schema");
    println!("cargo:rerun-if-env-changed=CAPNP");
    let mut compiler = capnpc::CompilerCommand::new();
    compiler.src_prefix("schema").import_path("schema");
    if let Ok(path) = std::env::var("CAPNP") {
        compiler.capnp_executable(path);
    }
    for file in [
        "sandstorm/api-session.capnp",
        "sandstorm/isolate-bridge.capnp",
        "sandstorm/outbound-http-session.capnp",
        "supervisor-probe.capnp",
        "route-session.capnp",
        "sandstorm/grain.capnp",
        "sandstorm/web-session.capnp",
        "sandstorm/util.capnp",
        "sandstorm/identity.capnp",
        "sandstorm/activity.capnp",
        "sandstorm/powerbox.capnp",
        "isolate-host.capnp",
        "isolate-worker-source.capnp",
        "capnp/compat/byte-stream.capnp",
        "capnp/compat/http-over-capnp.capnp",
    ] {
        compiler.file(format!("schema/{file}"));
    }
    compiler
        .run()
        .expect("install the standard capnp compiler or set CAPNP");
}
