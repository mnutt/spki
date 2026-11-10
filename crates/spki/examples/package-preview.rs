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
    #[arg(long, default_value = "build/grain-root")]
    runtime_root: PathBuf,
    #[arg(long)]
    test: bool,
    #[arg(long)]
    keyring: Option<PathBuf>,
    /// Test identity matching the supplied test keyring.
    #[arg(long, requires = "keyring", requires = "test")]
    app_id: Option<String>,
    #[arg(long, requires = "test")]
    version: Option<String>,
    #[arg(long, requires = "test")]
    app_version: Option<u32>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output.exists(), "output already exists");
    ensure!(
        args.runtime_root.join("bin/isolate-preview").is_file(),
        "stage the previewer first"
    );
    ensure!(
        args.runtime_root.join("bin/isolate-host").is_file(),
        "stage the isolate host first"
    );
    // Parse the existing package definition so manifest versions and commands
    // remain shared with local development. This uses the schema compiler only,
    // not the spk command or a running server.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let definition = spki::capnp_compiler::Configuration::read(
        &root.join(".sandstorm/sandstorm-pkgdef.capnp"),
        &root.join("crates/spki/schema"),
    )?;
    let package = definition.package()?;
    let manifest_bytes = definition.manifest()?;
    let temporary = tempfile::tempdir()?;
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
        spk::identity(&spk::keyring(args.keyring)?, package["id"].as_str().context("package ID missing")?)?
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
        if let Some(version) = args.version {
            value
                .reborrow()
                .init_app_marketing_version()
                .set_default_text(&version);
        }
        if let Some(version) = args.app_version {
            value.set_app_version(version);
        }
    }
    let stage = temporary.path().join("stage");
    spki::runtime::stage(&args.runtime_root, &stage)?;
    fs::write(stage.join("app/worker.js"), include_bytes!("../../../preview/fixtures/sdk-counter.js"))?;
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
