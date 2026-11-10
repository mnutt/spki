//! Native previewer packaging, sharing the CLI's SPK writer and verifier.
use spki::{package_capnp, spk};

use anyhow::{Context, Result, ensure};
use capnp::{message::Builder, serialize};
use clap::Parser;
use std::{fs, path::PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    runtime_root: Option<PathBuf>,
    #[arg(long)]
    test: bool,
    #[arg(long)]
    keyring: Option<PathBuf>,
    /// Test identity matching the supplied test keyring.
    #[arg(long, requires = "keyring", requires = "test")]
    app_id: Option<String>,
    #[arg(long, requires = "test")]
    app_version: Option<u32>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output.exists(), "output already exists");
    // Parse the existing package definition so manifest versions and commands
    // remain shared with local development. This uses the schema compiler only,
    // not the spk command or a running server.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let definition = spki::capnp_compiler::Configuration::read(
        &root.join(".sandstorm/sandstorm-pkgdef.capnp"),
        &root.join("crates/spki/schema"),
    )?;
    let package = definition.package()?;
    ensure!(
        package["manifest"]["appMarketingVersion"]["defaultText"].as_str()
            == Some(env!("CARGO_PKG_VERSION")),
        "preview appMarketingVersion must match the Cargo workspace version ({})",
        env!("CARGO_PKG_VERSION")
    );
    let manifest_bytes = definition.manifest()?;
    let parent = args
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = tempfile::tempdir_in(parent)?;
    let signing = if args.test {
        if let Some(id) = &args.app_id {
            spk::identity(
                args.keyring
                    .as_deref()
                    .context("test app ID requires a keyring")?,
                id,
            )?
        } else {
            ensure!(
                args.keyring.is_none(),
                "supply --app-id with a test keyring"
            );
            let keyring = temporary.path().join("test-keyring");
            let id = spk::keygen(&keyring)?;
            spk::identity(&keyring, &id)?
        }
    } else {
        spk::identity(
            &spk::keyring(args.keyring)?,
            package["id"].as_str().context("package ID missing")?,
        )?
    };
    let mut manifest = Builder::new_default();
    let input = serialize::read_message(manifest_bytes.as_slice(), Default::default())?;
    manifest.set_root(input.get_root::<package_capnp::manifest::Reader>()?)?;
    if args.test {
        let mut value = manifest.get_root::<package_capnp::manifest::Builder>()?;
        value
            .reborrow()
            .init_app_title()
            .set_default_text("Isolate Preview (test)");
        if let Some(version) = args.app_version {
            value.set_app_version(version);
        }
    }
    let stage = temporary.path().join("stage");
    match args.runtime_root {
        Some(root) => spki::runtime::stage(&root, &stage)?,
        None => spki::runtime::stage_embedded(&stage)?,
    }
    fs::write(
        stage.join("app/worker.js"),
        include_bytes!("../../../preview/fixtures/sdk-counter.js"),
    )?;
    fs::write(
        stage.join("sandstorm-manifest"),
        serialize::write_message_to_words(&manifest),
    )?;
    if let Some(parent) = args.output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    spk::write(&stage, &signing, &args.output)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&spk::verify(&args.output)?)?
    );
    Ok(())
}
