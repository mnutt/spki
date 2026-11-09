use anyhow::{Context, Result, ensure};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{BufRead, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};

pub(crate) fn prompt_webkey() -> Result<String> {
    let mut prompt = std::io::stderr().lock();
    write!(prompt, "Paste your Developer webkey and press Enter:\n> ")?;
    prompt.flush()?;
    let mut input = String::new();
    std::io::stdin().lock().take(16385).read_line(&mut input)?;
    ensure!(input.len() <= 16384, "webkey too long");
    ensure!(!input.trim().is_empty(), "no webkey supplied");
    Ok(input)
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Credential {
    pub(crate) endpoint: String,
    pub(crate) token: String,
    pub(crate) allow_local_http: bool,
}
pub(crate) fn sandstorm_loopback(host: &str) -> bool {
    host == "local.sandstorm.io" || host.ends_with(".local.sandstorm.io")
}
pub(crate) fn parse_webkey(input: &str, local: bool) -> Result<Credential> {
    let mut url = Url::parse(input.trim()).map_err(|_| anyhow::anyhow!("invalid webkey URL"))?;
    ensure!(
        url.username().is_empty() && url.password().is_none() && url.query().is_none(),
        "webkey must not contain URL credentials or a query"
    );
    let host = url
        .host_str()
        .unwrap_or("")
        .trim_matches(['[', ']'])
        .to_owned();
    let loopback = sandstorm_loopback(&host)
        || host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    ensure!(
        url.scheme() == "https" || (local && url.scheme() == "http" && loopback),
        "HTTPS required; local HTTP requires --allow-local-http and a loopback endpoint"
    );
    let token = url.fragment().unwrap_or("").to_owned();
    ensure!(
        !token.is_empty() && token.len() <= 8192 && token.bytes().all(|b| b.is_ascii_graphic()),
        "missing or invalid webkey token"
    );
    url.set_fragment(None);
    ensure!(
        url.path() == "/",
        "webkey endpoint must use the API origin root"
    );
    Ok(Credential {
        endpoint: url.to_string(),
        token,
        allow_local_http: local,
    })
}
pub(crate) fn store_path(profile: &str) -> Result<PathBuf> {
    ensure!(
        !profile.is_empty()
            && profile.len() <= 64
            && profile
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "profile must contain only letters, numbers, hyphens or underscores"
    );
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is unset")?).join(".config"),
    };
    ensure!(base.is_absolute(), "credential directory must be absolute");
    Ok(base.join("spki").join(format!("{profile}.json")))
}
pub(crate) fn save(path: &std::path::Path, credential: &Credential) -> Result<()> {
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent)?;
    ensure!(
        !fs::symlink_metadata(parent)?.file_type().is_symlink(),
        "credential directory must not be a symlink"
    );
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(&serde_json::to_vec(credential)?)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
pub(crate) fn load(path: &std::path::Path) -> Result<Credential> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .context("no readable credential; run spki auth")?;
    ensure!(
        file.metadata()?.permissions().mode() & 0o077 == 0,
        "credential file permissions must be 0600"
    );
    let mut bytes = Vec::new();
    (&mut file).take(32769).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 32768, "credential file too large");
    let saved: Credential =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid credential file"))?;
    parse_webkey(
        &format!("{}#{}", saved.endpoint, saved.token),
        saved.allow_local_http,
    )
}
