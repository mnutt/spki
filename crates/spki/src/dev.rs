//! Change detection and serial deployment. Idle polls never invoke a compiler.
use crate::{
    api::{Api, DeploymentFailure, deploy, revision},
    compiler::Compiler,
    credentials::{load, store_path},
    project,
};
use anyhow::{Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};

// mtime plus ctime/inode catches atomic saves and same-size replacements without reading
// every dependency on every poll. Directory enumeration discovers newly created imports.
#[derive(Debug, PartialEq, Eq)]
struct Stamp(u64, u64, i64, i64, i64, i64);
type Snapshot = BTreeMap<PathBuf, Stamp>;
struct Watch {
    roots: BTreeSet<PathBuf>,
    inputs: BTreeSet<PathBuf>,
    assets: Vec<PathBuf>,
}
impl Watch {
    fn snapshot(&self) -> Result<Snapshot> {
        fn visit(
            path: &Path,
            out: &mut Snapshot,
            seen: &mut BTreeSet<PathBuf>,
            all: bool,
        ) -> Result<()> {
            let metadata = match fs::metadata(path) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e.into()),
            };
            if metadata.is_dir() {
                if !seen.insert(fs::canonicalize(path)?) {
                    return Ok(());
                }
                for entry in fs::read_dir(path)? {
                    let entry = entry?;
                    if !all
                        && matches!(
                            entry.file_name().to_str(),
                            Some(".git" | "dist" | "target" | "build" | ".spki")
                        )
                    {
                        continue;
                    }
                    let path = entry.path();
                    if all
                        || entry.file_type()?.is_dir()
                        || path.is_dir()
                        || path.extension().is_some_and(|extension| {
                            matches!(
                                extension.to_str(),
                                Some(
                                    "js" | "mjs"
                                        | "cjs"
                                        | "jsx"
                                        | "ts"
                                        | "tsx"
                                        | "mts"
                                        | "cts"
                                        | "json"
                                        | "css"
                                        | "capnp"
                                )
                            )
                        })
                    {
                        visit(&path, out, seen, all)?;
                    }
                }
            } else if metadata.is_file() {
                out.insert(
                    path.to_owned(),
                    Stamp(
                        metadata.ino(),
                        metadata.len(),
                        metadata.mtime(),
                        metadata.mtime_nsec(),
                        metadata.ctime(),
                        metadata.ctime_nsec(),
                    ),
                );
            }
            Ok(())
        }
        let mut snapshot = Snapshot::new();
        let mut seen = BTreeSet::new();
        for root in &self.roots {
            visit(root, &mut snapshot, &mut seen, false)?;
        }
        for input in &self.inputs {
            visit(input, &mut snapshot, &mut seen, false)?;
            // Include resolution configuration for dependencies outside the project tree.
            for directory in input.ancestors().skip(1) {
                for name in ["package.json", "tsconfig.json", "jsconfig.json"] {
                    visit(&directory.join(name), &mut snapshot, &mut seen, false)?;
                }
            }
        }
        let mut seen = BTreeSet::new();
        for root in &self.assets {
            visit(root, &mut snapshot, &mut seen, true)?;
        }
        Ok(snapshot)
    }
}
fn config_changed(before: &Snapshot, after: &Snapshot) -> bool {
    let config = |entry: &(&PathBuf, &Stamp)| {
        entry
            .0
            .extension()
            .is_some_and(|extension| extension == "capnp")
    };
    !before.iter().filter(config).eq(after.iter().filter(config))
}
pub(crate) fn run(
    config: Option<&Path>,
    entry: Option<&Path>,
    profile: Option<&str>,
    once: bool,
) -> Result<()> {
    let mut settings = project::resolve(config, entry, profile)?;
    let compiler = Compiler::new()?;
    let mut api = Api::new(load(&store_path(&settings.profile)?)?)?;
    if !settings.project.is_empty() {
        api.select_project(&settings.project)?;
    }
    let connected_project = settings.project.clone();
    let connected_profile = settings.profile.clone();
    let info = api.info()?;
    ensure!(
        info.log_stream,
        "Preview does not support log streaming; update the Preview app"
    );
    let _logs = crate::logs::Follower::start(api.credential.clone())?;
    let limit = info.max_source_bytes;
    let mut expected = revision(&info.revision)?;
    let mut watch = Watch {
        roots: BTreeSet::new(),
        inputs: settings.binding_paths()?.into_iter().collect(),
        assets: settings.assets.clone(),
    };
    if let Some(config) = config {
        watch.roots.insert(config.parent().unwrap().to_owned());
    }
    watch.roots.insert(
        std::path::absolute(&settings.entry)?
            .parent()
            .unwrap()
            .to_owned(),
    );
    let mut previous: Option<Snapshot> = None;
    let mut observed = String::new();
    let mut config_valid = true;
    loop {
        let before = watch.snapshot()?;
        if previous.as_ref() == Some(&before) {
            std::thread::sleep(Duration::from_millis(300));
            continue;
        }
        let refresh_config = !config_valid
            || previous
                .as_ref()
                .is_some_and(|old| config_changed(old, &before));
        let mut unstable = false;
        let result = (|| -> Result<()> {
            if refresh_config {
                config_valid = false;
                settings = project::resolve(config, entry, profile)?;
                watch.inputs.extend(settings.binding_paths()?);
                config_valid = true;
            }
            ensure!(
                settings.profile == connected_profile && settings.project == connected_project,
                "preview profile or project changed; restart spki dev to reconnect"
            );
            watch.roots.insert(
                std::path::absolute(&settings.entry)?
                    .parent()
                    .unwrap()
                    .to_owned(),
            );
            watch.assets = settings.assets.clone();
            let code = compiler.build_project(&settings, limit)?;
            if !once {
                std::thread::sleep(Duration::from_millis(200));
                if watch.snapshot()? != before {
                    unstable = true;
                    return Ok(());
                }
                // A second build covers edits to dependencies first discovered by this build.
                if compiler.build_project(&settings, limit)? != code {
                    unstable = true;
                    return Ok(());
                }
            }
            if code != observed {
                observed = code.clone();
                deploy(&api, &code, &mut expected)?;
            }
            Ok(())
        })();
        // Retain the pre-build snapshot on changes during compilation; retry on the next poll.
        let stable = watch.snapshot()? == before;
        previous = if unstable { None } else { Some(before) };
        if stable && !unstable {
            watch.inputs.extend(compiler.inputs());
            previous = Some(watch.snapshot()?);
        }
        if let Err(error) = result {
            if once || error.is::<DeploymentFailure>() {
                return Err(error);
            }
            eprintln!(
                "Deployment failed: {error}. Last confirmed revision {} remains selected; edit the project to retry.",
                &expected[..12]
            );
        }
        if once {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn watches_imports_config_and_external_inputs_but_ignores_logs() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let external = tempfile::tempdir()?;
        let dependency = external.path().join("dependency.ts");
        fs::write(&dependency, "export const n = 1")?;
        let watch = Watch {
            roots: BTreeSet::from([dir.path().to_owned()]),
            inputs: BTreeSet::from([dependency.clone()]),
            assets: Vec::new(),
        };
        let before = watch.snapshot()?;
        fs::write(dir.path().join("watch.log"), "Live revision")?;
        assert_eq!(before, watch.snapshot()?);
        fs::write(dir.path().join("missing.ts"), "export const m = 1")?;
        let imported = watch.snapshot()?;
        assert_ne!(before, imported);
        assert!(!config_changed(&before, &imported));
        fs::write(dir.path().join("sandstorm-pkgdef.capnp"), "config")?;
        let configured = watch.snapshot()?;
        assert!(config_changed(&imported, &configured));
        fs::write(dependency, "export const n = 2")?;
        assert_ne!(configured, watch.snapshot()?);
        Ok(())
    }
}
