//! Compile explicit application schemas with the same pinned capnp-es used by the runtime.
use crate::project::Settings;
use anyhow::{Result, ensure};
use std::{collections::BTreeMap, fs, path::Path};

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
    let mut files = settings.schemas.clone();
    let mut bytes = crate::capnp_compiler::compile(&files, &settings.root, &imports)?;
    // capnp-es emits requested files only. Include the transitive local file nodes
    // from capnp's parsed graph; never discover imports by scanning source text.
    let message = capnp::serialize::read_message(&mut bytes.as_slice(), Default::default())?;
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
        files.extend(dependencies);
        bytes = crate::capnp_compiler::compile(&files, &settings.root, &imports)?;
    }
    let files = crate::schema_engine::generate(&bytes)?;
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
