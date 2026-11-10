#![forbid(unsafe_code)]
use spki::*;
mod api;
mod compiler;
mod credentials;
mod dev;
mod frontend;
mod logs;
mod schema_engine;
mod schemas;
use api::{Api, revision};
use credentials::{load, parse_webkey, save, store_path};
mod pack;
mod project;
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
};

#[derive(Parser)]
#[command(
    version,
    about = "Build and deploy TypeScript/JavaScript projects to an Isolate Preview workspace"
)]
struct Args {
    #[arg(long, global = true)]
    profile: Option<String>,
    /// Project directory or sandstorm-pkgdef.capnp; otherwise search current directory and parents.
    #[arg(long, global = true)]
    project: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Generate a signing identity in an SDK-compatible private keyring.
    Keygen {
        #[arg(long)]
        keyring: Option<PathBuf>,
    },
    /// Verify SPK signatures and archive structure without running package code.
    Verify { file: PathBuf },
    /// Build and sign an ordinary production SPK. Does not install or upload it.
    Pack {
        #[arg(long)]
        output: PathBuf,
        /// Prepared Linux runtime bundle from scripts/stage-preview.py.
        #[arg(long)]
        runtime_root: PathBuf,
        #[arg(long)]
        keyring: Option<PathBuf>,
        #[arg(long)]
        app_id: Option<String>,
        #[arg(long, default_value = "linux-x86_64")]
        target: String,
    },
    /// Create a new TypeScript project in a new or empty directory.
    Init {
        #[arg(default_value = ".")]
        directory: PathBuf,
    },
    /// Prompt for a deployment webkey, validate it, and store it (no source is uploaded).
    Auth {
        #[arg(long, conflicts_with = "webkey_stdin")]
        webkey: Option<String>,
        #[arg(long)]
        webkey_stdin: bool,
        /// Permit plaintext HTTP only for loopback, *.localhost or *.local.sandstorm.io endpoints.
        #[arg(long)]
        allow_local_http: bool,
        #[command(subcommand)]
        action: Option<AuthAction>,
    },
    /// Delete the selected local credential. Does not revoke the server token.
    Logout,
    /// Bundle TS/JS and local dependencies, then watch for changes. Does not type-check.
    Dev {
        #[arg(long)]
        entry: Option<PathBuf>,
        #[arg(long)]
        once: bool,
    },
    /// Compile a worker locally without Preview auth. Uses the same compiler as dev.
    Build {
        #[arg(long)]
        entry: Option<PathBuf>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
}
#[derive(Subcommand)]
enum AuthAction {
    Status,
}
fn run() -> Result<()> {
    let args = Args::parse();
    if let Command::Init { directory } = &args.command {
        ensure!(
            args.project.is_none(),
            "init takes a directory, not --project"
        );
        return project::init(directory, args.profile.as_deref().unwrap_or("default"));
    }
    match &args.command {
        Command::Keygen { keyring } => {
            println!("{}", spk::keygen(&spk::keyring(keyring.clone())?)?);
            return Ok(());
        }
        Command::Verify { file } => {
            println!("{}", serde_json::to_string_pretty(&spk::verify(file)?)?);
            return Ok(());
        }
        _ => (),
    }
    let config = project::discover(args.project.as_deref())?;
    let profile = if let Some(profile) = &args.profile {
        profile.clone()
    } else if let Some(file) = &config {
        project::read(file)?.profile
    } else {
        "default".into()
    };
    match args.command {
        Command::Init { .. } | Command::Keygen { .. } | Command::Verify { .. } => unreachable!(),
        Command::Pack {
            output,
            runtime_root,
            keyring,
            app_id,
            target,
        } => {
            pack::pack(
                config
                    .as_deref()
                    .context("pack requires a project configuration")?,
                pack::Options {
                    output,
                    runtime_root,
                    keyring: spk::keyring(keyring)?,
                    app_id,
                    target,
                },
            )?;
        }
        Command::Logout => {
            let path = store_path(&profile)?;
            match fs::remove_file(path) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
            println!("Local credential removed; server token is unchanged.");
        }
        Command::Auth {
            webkey,
            webkey_stdin,
            allow_local_http,
            action,
        } => {
            let path = store_path(&profile)?;
            if action.is_some() {
                ensure!(
                    webkey.is_none() && !webkey_stdin,
                    "auth status does not accept a webkey"
                );
                let info = Api::new(load(&path)?)?.info()?;
                if info.workspace {
                    println!("Profile {}: workspace deployment access verified", profile);
                } else {
                    println!(
                        "Profile {}: deployment access verified; revision {}",
                        profile,
                        &revision(&info.revision)?[..12]
                    );
                }
            } else {
                let input = if webkey_stdin {
                    let mut input = String::new();
                    std::io::stdin().take(16385).read_to_string(&mut input)?;
                    ensure!(input.len() <= 16384, "webkey too long");
                    input
                } else if let Some(webkey) = webkey {
                    webkey
                } else {
                    credentials::prompt_webkey()?
                };
                let api = Api::new(parse_webkey(&input, allow_local_http)?)?;
                api.info()?;
                save(&path, &api.credential)?;
                println!(
                    "Saved deployment profile {}. Projects using this profile share the Preview workspace.",
                    profile
                );
            }
        }
        Command::Build { entry, output } => {
            let settings =
                project::resolve(config.as_deref(), entry.as_deref(), args.profile.as_deref())?;
            let code = compiler::Compiler::new()?.build_project(&settings, 8 * 1024 * 1024)?;
            let output = match output {
                Some(output) => {
                    ensure!(
                        !output.exists(),
                        "output already exists; choose a fresh output path"
                    );
                    output
                }
                None => config
                    .as_ref()
                    .context("--output is required outside a project")?
                    .parent()
                    .unwrap()
                    .join("dist/worker.js"),
            };
            let parent = output
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));
            fs::create_dir_all(parent)?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(code.as_bytes())?;
            file.persist(&output)?;
            println!("Built {} bytes: {}", code.len(), output.display());
        }
        Command::Dev { entry, once } => {
            dev::run(
                config.as_deref(),
                entry.as_deref(),
                args.profile.as_deref(),
                once,
            )?;
        }
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("spki: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn webkeys_require_explicit_local_http_and_root() {
        assert!(parse_webkey("https://api.example/#secret", false).is_ok());
        assert!(parse_webkey("http://localhost:6091/#secret", false).is_err());
        assert!(parse_webkey("http://localhost:6091/#secret", true).is_ok());
        assert!(parse_webkey("http://api-test.local.sandstorm.io:6091/#secret", true).is_ok());
        assert!(parse_webkey("http://api-test.local.sandstorm.io:6091/#secret", false).is_err());
        assert!(parse_webkey("http://local.sandstorm.io.evil.test/#secret", true).is_err());
        for bad in [
            "http://example.com/#secret",
            "https://user:secret@example.com/#token",
            "https://example.com/?token=secret#token",
            "https://example.com/path#token",
            "https://example.com/",
        ] {
            assert!(parse_webkey(bad, true).is_err());
        }
    }
    #[test]
    fn credential_file_is_private_and_rejects_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth/default.json");
        let credential = parse_webkey("https://example.com/#secret", false).unwrap();
        save(&path, &credential).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(load(&path).unwrap().token, "secret");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(load(&link).is_err());
    }
}
