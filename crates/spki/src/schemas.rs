//! Compile explicit application schemas with the same pinned capnp-es used by the runtime.
use crate::project::Settings;
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub fn compile(settings: &Settings, directory: &Path) -> Result<BTreeMap<String, String>> {
    if settings.schemas.is_empty() {
        return Ok(BTreeMap::new());
    }
    let imports = directory.join("imports");
    for (name, source) in crate::project::SCHEMAS {
        let path = imports.join(name);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, source)?;
    }
    let mut command = Command::new(std::env::var_os("CAPNP").unwrap_or_else(|| "capnp".into()));
    command
        .current_dir(&settings.root)
        .args(["compile", "--no-standard-import", "-o-"])
        .arg(format!("-I{}", imports.display()))
        .arg(format!("--src-prefix={}", settings.root.display()))
        .args(&settings.schemas);
    let mut result = command.output().context("could not run schema compiler")?;
    ensure!(
        result.status.success(),
        "schema compilation failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    // capnp-es emits requested files only. Include the transitive local file nodes
    // from capnp's parsed graph; never discover imports by scanning source text.
    let message =
        capnp::serialize::read_message(&mut result.stdout.as_slice(), Default::default())?;
    let request = message.get_root::<capnp::schema_capnp::code_generator_request::Reader>()?;
    let mut dependencies = std::collections::BTreeSet::new();
    for node in request.get_nodes()? {
        if matches!(node.which()?, capnp::schema_capnp::node::File(())) {
            let name = node.get_display_name()?.to_str()?;
            let path = settings.root.join(name);
            if path.is_file() && !settings.schemas.contains(&path) {
                ensure!(
                    path.canonicalize()?
                        .starts_with(settings.root.canonicalize()?),
                    "schema import escapes project root"
                );
                dependencies.insert(path);
            }
        }
    }
    if !dependencies.is_empty() {
        result = command.args(dependencies).output()?;
        ensure!(
            result.status.success(),
            "schema import compilation failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    // This pinned tool is installed independently of any Sandstorm source checkout.
    let compiler = std::env::var_os("SPKI_SCHEMA_COMPILER")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tools/schema-compiler/compile.mjs")
        });
    let input = directory.join("request.bin");
    fs::File::create(&input)?.write_all(&result.stdout)?;
    let output = Command::new("node")
        .arg(compiler)
        .stdin(Stdio::from(fs::File::open(input)?))
        .output()
        .context(
            "schema compilation requires Node.js and the pinned tools/schema-compiler package",
        )?;
    ensure!(
        output.status.success(),
        "capnp-es compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let files: Vec<(String, String)> = serde_json::from_slice(&output.stdout)?;
    let mut modules = BTreeMap::new();
    for (name, source) in files {
        if let Some(stem) = name.strip_suffix(".js") {
            ensure!(
                Path::new(stem)
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
                "invalid generated schema path"
            );
            modules.insert(format!("{stem}.capnp"), source);
        }
    }
    Ok(modules)
}
