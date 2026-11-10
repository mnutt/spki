use crate::{compiler::Compiler, ids, package_capnp as p, project, spk};
use anyhow::{Context, Result, ensure};
use capnp::{message::Builder, serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
fn command(mut command: p::manifest::command::Builder<'_>) {
    let mut argv = command.reborrow().init_argv(4);
    for (i, arg) in [
        "/bin/isolate-preview",
        "app",
        "/bin/isolate-host",
        "/app/worker.js",
    ]
    .iter()
    .enumerate()
    {
        argv.set(i as u32, *arg);
    }
}
pub struct Options {
    pub output: PathBuf,
    pub runtime_root: PathBuf,
    pub keyring: PathBuf,
    pub app_id: Option<String>,
    pub target: String,
}
pub fn pack(file: &Path, options: Options) -> Result<()> {
    ensure!(
        options.target == "linux-x86_64",
        "only linux-x86_64 packages are supported"
    );
    ensure!(
        !options.output.exists(),
        "output already exists; choose a fresh release filename"
    );
    let settings = project::resolve(Some(file), None, None)?;
    let (metadata, bytes) = project::package_metadata(file)?;
    let configured = metadata["id"].as_str().filter(|id| !id.is_empty());
    if let (Some(configured), Some(requested)) = (configured, options.app_id.as_deref()) {
        ensure!(configured == requested, "--app-id conflicts with pkgdef.id");
    }
    let id = options.app_id.as_deref().or(configured).context(
        "set pkgdef.id or --app-id; create a new signing identity explicitly with spki keygen",
    )?;
    let key = spk::identity(&options.keyring, id)?;
    let code = Compiler::new()?
        .build_project(&settings, 8 * 1024 * 1024)?
        .code;
    let mut input = bytes.as_slice();
    let reader = serialize::read_message_from_flat_slice(
        &mut input,
        spk::limits(p::manifest::SIZE_LIMIT_IN_WORDS as usize),
    )?;
    ensure!(input.is_empty(), "trailing manifest data");
    let mut manifest = Builder::new_default();
    manifest.set_root(reader.get_root::<p::manifest::Reader>()?)?;
    let title;
    {
        let mut m = manifest.get_root::<p::manifest::Builder>()?;
        title = m
            .reborrow()
            .get_app_title()?
            .get_default_text()?
            .to_str()?
            .to_owned();
        let actions = m.reborrow().get_actions()?;
        ensure!(
            actions.len() == 1,
            "pack currently supports exactly one creation action"
        );
        let mut action = actions.get(0);
        ensure!(
            matches!(
                action.reborrow().get_input().which()?,
                p::manifest::action::input::None(())
            ),
            "pack currently supports only a no-input creation action"
        );
        ensure!(!action.has_command(), "action command is reserved for spki");
        command(action.init_command());
        ensure!(
            !m.has_continue_command(),
            "continue command is reserved for spki"
        );
        command(m.init_continue_command());
    }
    let stage = tempfile::tempdir()?;
    spki::runtime::stage(&options.runtime_root, stage.path())?;
    fs::write(stage.path().join("app/worker.js"), code.as_bytes())?;
    fs::write(
        stage.path().join("app/config.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"title":title,"revision":spki_common::revision(code.as_bytes())}),
        )?,
    )?;
    fs::write(
        stage.path().join("sandstorm-manifest"),
        serialize::write_message_to_words(&manifest),
    )?;
    println!(
        "Signing {} for app {}",
        options.output.display(),
        ids::app_id(key.verifying_key().as_bytes())
    );
    spk::write(stage.path(), &key, &options.output)?;
    println!("Verified {}", options.output.display());
    Ok(())
}
