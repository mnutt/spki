//! Compiler boundary: the provider receives ordinary ES module bytes, never bundler metadata.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub struct Compiler {
    pub(crate) binary: PathBuf,
    pub(crate) modules: BTreeSet<String>,
    pub(crate) schemas: std::cell::RefCell<std::collections::BTreeMap<String, PathBuf>>,
    pub(crate) inputs: std::cell::RefCell<BTreeSet<PathBuf>>,
}
impl Compiler {
    pub fn new(binary: PathBuf) -> Result<Self> {
        // Resolve explicit relative paths before changing to the project's source directory.
        let binary = if binary.components().count() > 1 {
            fs::canonicalize(binary)?
        } else {
            binary
        };
        let pin: Value = serde_json::from_str(include_str!("../../../runtime/esbuild.json"))?;
        let output = Command::new(&binary).arg("--version").output()
            .context("esbuild unavailable; run scripts/fetch-esbuild.py and pass --esbuild build/tools/esbuild")?;
        ensure!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).trim()
                    == pin["version"].as_str().unwrap(),
            "esbuild version mismatch; use the pinned compiler from scripts/fetch-esbuild.py"
        );
        let sdk: Value =
            serde_json::from_str(include_str!("../../isolate-preview/sdk/modules.json"))?;
        let modules = sdk
            .as_object()
            .context("invalid SDK module table")?
            .keys()
            .cloned()
            .collect();
        Ok(Self {
            binary,
            modules,
            inputs: Default::default(),
            schemas: Default::default(),
        })
    }
    pub(crate) fn runtime_aliases(&self, command: &mut Command, browser: bool) {
        let prefix = if browser { "/" } else { "" };
        command.arg(format!(
            "--alias:@mnutt/capnp-es={prefix}capnp-es/index.mjs"
        ));
        for name in &self.modules {
            if let Some(relative) = name.strip_prefix("capnp-es/") {
                command.arg(format!(
                    "--alias:@mnutt/capnp-es/{}={prefix}{name}",
                    relative.trim_end_matches(".mjs")
                ));
            }
        }
    }
    pub fn inputs(&self) -> BTreeSet<PathBuf> {
        self.inputs.borrow().clone()
    }
    pub fn build(&self, entry: &Path, limit: usize) -> Result<String> {
        let entry = fs::canonicalize(entry).context("cannot read worker entrypoint")?;
        let temporary = tempfile::tempdir()?;
        let output = temporary.path().join("worker.js");
        let meta = temporary.path().join("meta.json");
        let mut command = Command::new(&self.binary);
        command
            .current_dir(entry.parent().unwrap())
            .arg(format!(
                "./{}",
                entry.file_name().unwrap().to_string_lossy()
            ))
            .args([
                "--bundle",
                "--format=esm",
                "--platform=neutral",
                "--target=es2022",
                "--conditions=workerd,worker,browser",
                "--main-fields=module,main",
                "--charset=utf8",
                "--color=false",
                "--log-level=warning",
                "--log-override:unsupported-dynamic-import=error",
                "--log-override:unsupported-require-call=error",
            ])
            .arg(format!("--outfile={}", output.display()))
            .arg(format!("--metafile={}", meta.display()));
        for name in &self.modules {
            command.arg(format!("--external:{name}"));
        }
        self.runtime_aliases(&mut command, false);
        if !self.schemas.borrow().is_empty() {
            command.args(["--minify-whitespace", "--legal-comments=none"]);
        }
        for (name, path) in self.schemas.borrow().iter() {
            command.arg(format!("--alias:{name}={}", path.display()));
        }
        let result = command.output().context("could not launch compiler")?;
        let diagnostics: String = String::from_utf8_lossy(&result.stderr)
            .chars()
            .take(8192)
            .collect();
        ensure!(result.status.success(), "bundle failed:\n{diagnostics}");
        let metadata: Value = serde_json::from_slice(&fs::read(meta)?)?;
        if let Some(inputs) = metadata["inputs"].as_object() {
            *self.inputs.borrow_mut() = inputs
                .keys()
                .map(|path| entry.parent().unwrap().join(path))
                .collect();
        }
        let outputs = metadata["outputs"]
            .as_object()
            .context("missing compiler output metadata")?;
        ensure!(
            outputs.len() == 1,
            "worker must compile to one ES module; separate CSS/assets are not supported yet"
        );
        for file in outputs.values() {
            for import in file["imports"]
                .as_array()
                .context("missing compiler imports")?
            {
                ensure!(
                    import["path"]
                        .as_str()
                        .is_some_and(|p| self.modules.contains(p)),
                    "bundle contains an unsupported external import"
                );
            }
        }
        super::source(&output, limit)
    }
}
