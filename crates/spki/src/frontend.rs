//! Browser builds and public files become part of the same immutable deployment as the worker.
use crate::{
    compiler::{BuildResult, Compiler},
    project::Settings,
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

impl Compiler {
    pub fn build_project(&self, settings: &Settings, limit: usize) -> Result<BuildResult> {
        let mut result = self.build_application(settings, limit)?;
        let bindings = settings.bindings(&mut result.inputs)?;
        result.code = spki_common::bindings::attach(result.code, &bindings, limit)?;
        Ok(result)
    }
    fn build_application(&self, settings: &Settings, limit: usize) -> Result<BuildResult> {
        let generated = tempfile::tempdir()?;
        let schemas = crate::schemas::compile(settings, generated.path())?;
        let mut aliases = BTreeMap::new();
        for (name, source) in &schemas {
            let path = generated.path().join(format!("{name}.js"));
            fs::create_dir_all(path.parent().unwrap())?;
            let mut worker_source = source
                .replace("/capnp-es/", "capnp-es/")
                .replace("/__sandstorm/capnp/", "capnp:/");
            for platform in self
                .modules
                .iter()
                .filter(|name| name.starts_with("capnp:/sandstorm/") && name.ends_with(".capnp"))
            {
                worker_source = worker_source.replace(&format!("{platform}.js"), platform);
            }
            fs::write(&path, worker_source)?;
            aliases.insert(format!("capnp:./{name}"), path.clone());
        }
        let worker = self.build_worker(&settings.entry, limit, &aliases)?;
        if settings.ui.is_empty() && settings.assets.is_empty() && schemas.is_empty() {
            return Ok(worker);
        }
        let mut inputs = worker.inputs;
        let stage = tempfile::tempdir()?;
        let output = stage.path().join("assets");
        fs::create_dir(&output)?;
        if !settings.ui.is_empty() {
            inputs.extend(self.build_browser(&settings.ui, &settings.root, &output, &aliases)?);
        }
        let mut files = BTreeMap::new();
        let mut total = 0;
        collect(
            &output,
            &output,
            "assets",
            &mut files,
            &mut inputs,
            &mut total,
            limit,
        )?;
        for root in &settings.assets {
            ensure!(
                root.is_dir(),
                "asset directory not found: {}",
                root.display()
            );
            collect(root, root, "", &mut files, &mut inputs, &mut total, limit)?;
        }
        for (name, source) in schemas {
            files.insert(
                format!("__sandstorm/capnp/{name}.js"),
                (
                    "text/javascript; charset=utf-8".into(),
                    STANDARD.encode(source.as_bytes()),
                ),
            );
            files.insert(
                format!("__sandstorm/capnp/{name}"),
                (
                    "text/javascript; charset=utf-8".into(),
                    STANDARD.encode(source.as_bytes()),
                ),
            );
        }
        fs::write(stage.path().join("worker.mjs"), worker.code)?;
        let entry = format!(
            "import app from './worker.mjs';\nexport * from './worker.mjs';\nconst assets = {};\n{}",
            serde_json::to_string(&files)?,
            include_str!("../templates/assets.js")
        );
        let path = stage.path().join("entry.mjs");
        fs::write(&path, entry)?;
        let mut result = self.build_worker(&path, limit, &BTreeMap::new())?;
        inputs
            .retain(|path| !path.starts_with(stage.path()) && !path.starts_with(generated.path()));
        result.inputs = inputs;
        Ok(result)
    }
}
fn collect(
    root: &Path,
    directory: &Path,
    prefix: &str,
    files: &mut BTreeMap<String, (String, String)>,
    inputs: &mut BTreeSet<PathBuf>,
    total: &mut usize,
    limit: usize,
) -> Result<()> {
    ensure!(
        !fs::symlink_metadata(directory)?.file_type().is_symlink(),
        "asset directories must not be symlinks"
    );
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(root)?;
        let name = relative.to_str().context("asset path is not UTF-8")?;
        ensure!(
            relative.components().all(|c| c
                .as_os_str()
                .to_str()
                .is_some_and(|s| !s.starts_with('.')
                    && !s.contains(['\\', '%', '?', '#', ':'])
                    && !s.chars().any(char::is_control))),
            "unsafe or hidden asset path: {name}"
        );
        if entry.file_type()?.is_dir() {
            ensure!(
                relative.components().count() <= 32,
                "asset nesting exceeds limit"
            );
            collect(root, &path, prefix, files, inputs, total, limit)?;
        } else {
            ensure!(
                entry.file_type()?.is_file(),
                "assets must be regular files: {name}"
            );
            let route = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            ensure!(
                !route.starts_with("__spki")
                    && !route.starts_with("__sandstorm")
                    && !route.starts_with("capnp-es/"),
                "reserved asset path: {route}"
            );
            ensure!(
                files.len() < 2048 && entry.metadata()?.len() <= (1024 * 1024),
                "asset limit exceeded (2048 files, 1 MiB per file)"
            );
            use std::io::Read;
            let mut bytes = Vec::new();
            fs::File::open(&path)?
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 1024 * 1024, "asset exceeds 1 MiB");
            *total += bytes.len();
            ensure!(*total <= limit, "total assets exceed deployment limit");
            ensure!(
                files
                    .insert(route.clone(), (mime(&path).into(), STANDARD.encode(bytes)))
                    .is_none(),
                "duplicate asset URL: /{route}"
            );
            inputs.insert(path);
        }
    }
    Ok(())
}
fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|v| v.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
