//! In-process schema compilation and project constant evaluation.
use anyhow::{Context, Result, bail, ensure};
use capnp::{dynamic_value::Reader as Dynamic, introspect::Introspect};
use std::path::{Path, PathBuf};

pub fn compile(files: &[PathBuf], root: &Path, imports: &Path) -> Result<Vec<u8>> {
    let mut command = capnp_compiler::CompileCommand::new();
    command
        .no_standard_import()
        .src_prefix(root)
        .import_path(imports);
    for file in files {
        command.file(file);
    }
    command
        .run_bytes()
        .map_err(|error| anyhow::anyhow!("schema compilation failed: {error}"))
}

pub struct Configuration {
    message: capnp::message::Reader<capnp::serialize::OwnedSegments>,
}
impl Configuration {
    pub fn read(file: &Path, imports: &Path) -> Result<Self> {
        let bytes = compile(
            &[file.to_owned()],
            file.parent().context("configuration has no parent")?,
            imports,
        )?;
        Ok(Self {
            message: capnp::serialize::read_message(&mut bytes.as_slice(), Default::default())?,
        })
    }

    fn constant<T: Introspect>(&self, name: &str) -> Result<Dynamic<'_>> {
        use capnp::schema_capnp::{node, type_};
        let request = self
            .message
            .get_root::<capnp::schema_capnp::code_generator_request::Reader>()?;
        let file_id = request.get_requested_files()?.get(0).get_id();
        let nodes = request.get_nodes()?;
        let file = nodes
            .iter()
            .find(|node| node.get_id() == file_id)
            .context("missing configuration file node")?;
        let id = file
            .get_nested_nodes()?
            .iter()
            .find(|node| node.get_name().ok().and_then(|s| s.to_str().ok()) == Some(name))
            .with_context(|| format!("missing project constant {name}"))?
            .get_id();
        let node = nodes
            .iter()
            .find(|node| node.get_id() == id)
            .context("missing constant node")?;
        let node::Const(value) = node.which()? else {
            bail!("{name} must be a constant")
        };
        let type_::Struct(actual) = value.get_type()?.which()? else {
            bail!("{name} must be a struct constant")
        };
        let capnp::introspect::TypeVariant::Struct(expected) = T::introspect().which() else {
            unreachable!()
        };
        let expected: capnp::schema::StructSchema = expected.into();
        ensure!(
            actual.get_type_id() == expected.get_proto().get_id(),
            "incorrect type for {name}"
        );
        Ok(Dynamic::new(value.get_value()?, T::introspect())?)
    }

    pub fn project(&self) -> Result<serde_json::Value> {
        json(self.constant::<crate::project_capnp::project::Owned>("spki")?)
    }
    pub fn package(&self) -> Result<serde_json::Value> {
        json(self.constant::<crate::package_capnp::package_definition::Owned>("pkgdef")?)
    }
    pub fn manifest(&self) -> Result<Vec<u8>> {
        let package = self
            .constant::<crate::package_capnp::package_definition::Owned>("pkgdef")?
            .downcast_struct::<crate::package_capnp::package_definition::Owned>();
        let mut message = capnp::message::Builder::new_default();
        message.set_root(package.get_manifest()?)?;
        Ok(capnp::serialize::write_message_to_words(&message))
    }
}

// Match capnp eval's JSON presence semantics: omit null pointer fields and
// inactive union members, while retaining scalar defaults and explicit empties.
fn json(value: Dynamic<'_>) -> Result<serde_json::Value> {
    use serde_json::{Value, json};
    Ok(match value {
        Dynamic::Void => Value::Null,
        Dynamic::Bool(v) => json!(v),
        Dynamic::Int8(v) => json!(v),
        Dynamic::Int16(v) => json!(v),
        Dynamic::Int32(v) => json!(v),
        Dynamic::UInt8(v) => json!(v),
        Dynamic::UInt16(v) => json!(v),
        Dynamic::UInt32(v) => json!(v),
        Dynamic::Int64(v) => json!(v.to_string()),
        Dynamic::UInt64(v) => json!(v.to_string()),
        Dynamic::Float32(v) => json!(v),
        Dynamic::Float64(v) => json!(v),
        Dynamic::Text(v) => json!(v.to_str()?),
        Dynamic::Data(v) => json!(v),
        Dynamic::Enum(v) => json!(
            v.get_enumerant()?
                .context("unknown enum value")?
                .get_proto()
                .get_name()?
                .to_str()?
        ),
        Dynamic::List(v) => Value::Array(
            (0..v.len())
                .map(|i| json(v.get(i)?))
                .collect::<Result<_>>()?,
        ),
        Dynamic::Struct(v) => {
            let mut object = serde_json::Map::new();
            for field in v.get_schema().get_fields()? {
                if v.has(field)? {
                    object.insert(
                        field.get_proto().get_name()?.to_str()?.to_owned(),
                        json(v.get(field)?)?,
                    );
                }
            }
            Value::Object(object)
        }
        Dynamic::AnyPointer(_) | Dynamic::Capability(_) => {
            bail!("unsupported pointer in project metadata")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires the official capnp executable for differential testing"]
    fn schema_nodes_match_official_compiler() -> Result<()> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()?;
        for path in ["crates/spki/schema", "crates/isolate-protocol/schema"] {
            let root = repository.join(path);
            fn collect(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
                for entry in std::fs::read_dir(path)? {
                    let path = entry?.path();
                    if path.is_dir() {
                        collect(&path, files)?;
                    } else if path.extension().is_some_and(|e| e == "capnp") {
                        files.push(path);
                    }
                }
                Ok(())
            }
            let mut files = Vec::new();
            collect(&root, &mut files)?;
            files.sort();
            let mut command = capnp_compiler::CompileCommand::new();
            command.src_prefix(&root).import_path(&root);
            for file in &files {
                command.file(file);
            }
            let actual = command.run_bytes().map_err(|e| anyhow::anyhow!("{e}"))?;
            let output = std::process::Command::new("capnp")
                .args(["compile", "-o-"])
                .arg(format!("-I{}", root.display()))
                .arg(format!("--src-prefix={}", root.display()))
                .args(&files)
                .output()?;
            ensure!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let nodes =
                |bytes: &[u8]| -> Result<std::collections::BTreeMap<u64, (String, Vec<u8>)>> {
                    let message = capnp::serialize::read_message(&mut &*bytes, Default::default())?;
                    let request = message
                        .get_root::<capnp::schema_capnp::code_generator_request::Reader>()?;
                    let mut result = std::collections::BTreeMap::new();
                    for node in request.get_nodes()? {
                        // Compiler versions may add diagnostic-only fields (e.g. source
                        // spans) beyond this schema. Compare all fields known to the
                        // generated schema, preserving defaults, unions and constants.
                        let capnp::dynamic_value::Reader::Struct(source) = node.into() else {
                            unreachable!()
                        };
                        let mut normalized = capnp::message::Builder::new_default();
                        let capnp::dynamic_value::Builder::Struct(mut target) = normalized
                            .init_root::<capnp::schema_capnp::node::Builder>()
                            .into()
                        else {
                            unreachable!()
                        };
                        for field in source.get_schema().get_fields()? {
                            if source.has(field)? {
                                target.set(field, source.get(field)?)?;
                            }
                        }
                        let mut message = capnp::message::Builder::new_default();
                        message.set_root_canonical(
                            normalized.get_root_as_reader::<capnp::schema_capnp::node::Reader>()?,
                        )?;
                        result.insert(
                            node.get_id(),
                            (
                                node.get_display_name()?.to_str()?.to_owned(),
                                message.get_segments_for_output()[0].to_vec(),
                            ),
                        );
                    }
                    Ok(result)
                };
            let actual = nodes(&actual)?;
            let expected = nodes(&output.stdout)?;
            assert_eq!(
                actual.keys().collect::<Vec<_>>(),
                expected.keys().collect::<Vec<_>>()
            );
            for (id, (name, words)) in actual {
                assert_eq!(words, expected[&id].1, "{path}: {name}");
            }
        }
        Ok(())
    }

    // Maintainer oracle test: the application and ordinary tests need no capnp.
    #[test]
    #[ignore = "requires the official capnp executable for differential testing"]
    fn constants_match_official_compiler() -> Result<()> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()?;
        let imports = repository.join("crates/spki/schema");
        let temporary = tempfile::tempdir()?;
        let rich = temporary.path().join("metadata.capnp");
        let source =
            std::fs::read_to_string(repository.join("examples/hello/sandstorm-pkgdef.capnp"))?;
        std::fs::write(
            &rich,
            source.replace(
                "manifest = (",
                r#"manifest = (
            metadata = (author = (pgpSignature = 0x"00ff1234"),
              license = (openSource = apache2),
              icons = (appGrid = (svg = "<svg/>"), grain = (png = (dpi1x = 0x"010203")))),"#,
            ),
        )?;
        let mut files = vec![rich];
        for entry in std::fs::read_dir(repository.join("examples"))? {
            let file = entry?.path().join("sandstorm-pkgdef.capnp");
            if file.is_file() {
                files.push(file);
            }
        }
        for file in files {
            let config = Configuration::read(&file, &imports)?;
            for (name, actual) in [("spki", config.project()?), ("pkgdef", config.package()?)] {
                let output = std::process::Command::new("capnp")
                    .args(["eval", "--no-standard-import", "-ojson"])
                    .arg(format!("-I{}", imports.display()))
                    .arg(&file)
                    .arg(name)
                    .output()?;
                ensure!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let expected: serde_json::Value = serde_json::from_slice(&output.stdout)?;
                assert_eq!(actual, expected, "{} {name}", file.display());
            }
            let output = std::process::Command::new("capnp")
                .args(["eval", "--no-standard-import", "-b"])
                .arg(format!("-I{}", imports.display()))
                .arg(&file)
                .arg("pkgdef.manifest")
                .output()?;
            ensure!(
                output.status.success(),
                "official manifest evaluation failed"
            );
            let actual = config.manifest()?;
            let canonical = |bytes: &[u8]| -> Result<Vec<capnp::Word>> {
                Ok(
                    capnp::serialize::read_message(&mut &*bytes, Default::default())?
                        .canonicalize()?,
                )
            };
            assert_eq!(
                canonical(&actual)?,
                canonical(&output.stdout)?,
                "manifest: {}",
                file.display()
            );
        }
        Ok(())
    }
}
