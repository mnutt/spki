//! A subprocess keeps the memory measurement independent of other test allocations.
#![cfg(target_os = "linux")]
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn packing_child() -> Result<()> {
    let Some(root) = std::env::var_os("SPKI_MEMORY_TEST_ROOT") else {
        return Ok(());
    };
    let root = PathBuf::from(root);
    let stage = root.join("stage");
    fs::create_dir(&stage)?;
    // A sparse input exercises the full data path without committing a large fixture.
    fs::File::create(stage.join("large"))?.set_len(256 * 1024 * 1024)?;
    let mut manifest = capnp::message::Builder::new_default();
    manifest
        .init_root::<spki::package_capnp::manifest::Builder>()
        .init_app_title()
        .set_default_text("Memory test");
    fs::write(
        stage.join("sandstorm-manifest"),
        capnp::serialize::write_message_to_words(&manifest),
    )?;
    let output = root.join("large.spk");
    println!("Packing the sparse fixture");
    spki::spk::write(
        &stage,
        &ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
        &output,
    )?;
    println!("Verifying the package independently");
    assert_eq!(spki::spk::verify(&output)?["title"], "Memory test");
    let status = fs::read_to_string("/proc/self/status")?;
    let peak: u64 = status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))
        .context("missing peak RSS")?
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()?;
    println!("Peak RSS for packing and verifying 256 MiB: {peak} KiB");
    ensure!(
        peak < 128 * 1024,
        "packing allocated archive-sized memory: {peak} KiB"
    );
    Ok(())
}
#[test]
fn packaging_memory_does_not_scale_with_file_bytes() -> Result<()> {
    let root = tempfile::tempdir_in(std::env::current_dir()?)?;
    let log = root.path().join("log");
    let output = fs::File::create(&log)?;
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--exact", "packing_child", "--nocapture"])
        .env("SPKI_MEMORY_TEST_ROOT", root.path())
        .env("TMPDIR", root.path())
        .stdout(Stdio::from(output.try_clone()?))
        .stderr(Stdio::from(output))
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("memory regression timed out: {}", fs::read_to_string(&log)?);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let log = fs::read_to_string(log)?;
    ensure!(status.success(), "{log}");
    print!("{log}");
    Ok(())
}
