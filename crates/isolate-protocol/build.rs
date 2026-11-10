fn main() {
    println!("cargo:rerun-if-changed=schema");
    let mut compiler = capnp_compiler::CompileCommand::new();
    compiler.src_prefix("schema").import_path("schema");
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
    let request = compiler.run_bytes().expect("compile protocol schemas");
    capnpc::codegen::CodeGenerationCommand::new()
        .output_directory(std::env::var("OUT_DIR").unwrap())
        .run(request.as_slice())
        .expect("generate protocol bindings");
}
