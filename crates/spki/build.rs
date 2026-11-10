use std::{env, fs, path::Path};
fn main() {
    println!("cargo:rerun-if-changed=schema");
    fn walk(dir: &Path, files: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().is_some_and(|ext| ext == "capnp") {
                files.push(path.to_str().unwrap().to_owned());
            }
        }
    }
    let mut files = Vec::new();
    walk(Path::new("schema"), &mut files);
    files.sort();
    let mut command = capnp_compiler::CompileCommand::new();
    command.src_prefix("schema").import_path("schema");
    let mut modules = String::new();
    for file in &files {
        if file.ends_with("c++.capnp") {
            continue;
        }
        command.file(file);
        let path = Path::new(file);
        let name = path
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .replace('-', "_");
        let relative = path
            .strip_prefix("schema")
            .unwrap()
            .with_file_name(format!("{name}_capnp.rs"));
        modules.push_str(&format!("#[allow(clippy::all)] pub mod {name}_capnp {{ include!(concat!(env!(\"OUT_DIR\"), \"/{}\")); }}\n",relative.display()));
    }
    let request = command.run_bytes().expect("compile package schemas");
    capnpc::codegen::CodeGenerationCommand::new()
        .output_directory(env::var("OUT_DIR").unwrap())
        .run(request.as_slice())
        .expect("generate package bindings");
    let out = std::path::PathBuf::from(env::var("OUT_DIR").unwrap());
    let supervisor = out.join("sandstorm/supervisor_capnp.rs");
    let generated = fs::read_to_string(&supervisor).unwrap().replace(
        "type_::get_annotation_types)",
        "type_::get_annotation_types::<::capnp::any_pointer::Owned>)",
    );
    fs::write(supervisor, generated).unwrap();
    // capnpc maps CamelCase structs and lowerCamelCase annotations to the same Rust name.
    // Rename annotation-only Rust modules, preserving the public source schemas and wire IDs.
    for (file, name) in [
        ("sandstorm/package_capnp.rs", "category_info"),
        ("capnp/persistent_capnp.rs", "persistent"),
    ] {
        let path = out.join(file);
        let generated = fs::read_to_string(&path)
            .unwrap()
            .replace(
                &format!("pub mod {name} {{\n    pub const ID:"),
                &format!("pub mod {name}_annotation {{\n    pub const ID:"),
            )
            .replace(
                &format!("::{name}::get_type"),
                &format!("::{name}_annotation::get_type"),
            );
        fs::write(path, generated).unwrap();
    }
    fs::write(out.join("bindings.rs"), modules).unwrap();
    let mut table = String::from("pub(crate) const SCHEMAS: &[(&str, &str)] = &[\n");
    for file in files {
        table.push_str(&format!(
            "({:?}, include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/{file}\"))),\n",
            file.strip_prefix("schema/").unwrap()
        ));
    }
    table.push_str("];\n");
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("schemas.rs"),
        table,
    )
    .unwrap();
}
