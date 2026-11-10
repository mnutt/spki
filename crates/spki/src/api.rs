use crate::credentials::{Credential, sandstorm_loopback};
use anyhow::{Context, Result, bail, ensure};
use spki_common::protocol::{Activation, Error as ErrorResponse, Info, Revision};
use reqwest::{Url, blocking::Client};
use std::{io::Read, time::Duration};
pub(crate) struct Api {
    client: Client,
    pub(crate) credential: Credential,
}
impl Api {
    pub(crate) fn new(credential: Credential) -> Result<Self> {
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30));
        let url = Url::parse(&credential.endpoint)?;
        if url.scheme() == "http" && credential.allow_local_http {
            builder = builder.no_proxy();
            if let Some(host) = url.host_str().filter(|host| sandstorm_loopback(host)) {
                // Explicit local-dev policy: these names are always loopback, independent of DNS.
                builder = builder.resolve(
                    host,
                    std::net::SocketAddr::from((
                        [127, 0, 0, 1],
                        url.port_or_known_default().unwrap(),
                    )),
                );
            }
        }
        Ok(Self {
            client: builder.build()?,
            credential,
        })
    }

    fn request<T: serde::de::DeserializeOwned>(
        &self,
        route: &str,
        payload: Option<&serde_json::Value>,
    ) -> Result<T> {
        let url = format!("{}__spki/v1/{route}", self.credential.endpoint);
        let request = if let Some(body) = payload {
            self.client.post(url).json(body)
        } else {
            self.client.get(url)
        };
        let response = request
            .bearer_auth(&self.credential.token)
            .send()
            .map_err(|_| anyhow::anyhow!("Preview request failed (connection or timeout)"))?;
        let status = response.status();
        let mut body = Vec::new();
        response
            .take(65537)
            .read_to_end(&mut body)
            .map_err(|_| anyhow::anyhow!("Preview response interrupted"))?;
        ensure!(body.len() <= 65536, "Preview response too large");
        if !status.is_success() {
            // Treat remote text as untrusted and never echo bearer credentials or control characters.
            let message = serde_json::from_slice::<ErrorResponse>(&body)
                .ok()
                .map(|v| v.error)
                .unwrap_or_default()
                .replace(&self.credential.token, "[redacted]");
            let safe: String = message
                .chars()
                .filter(|c| !c.is_control())
                .take(2048)
                .collect();
            bail!("Preview HTTP {}: {}", status.as_u16(), safe);
        }
        serde_json::from_slice(&body).map_err(|_| anyhow::anyhow!("invalid Preview response"))
    }
    pub(crate) fn select_project(&mut self, id: &str) -> Result<()> {
        spki_common::validate_id(id)?;
        ensure!(
            self.info()?.workspace,
            "Preview does not support workspaces; update the Preview app"
        );
        let _: serde_json::Value = self.request(
            &format!("projects/{id}/create"),
            Some(&serde_json::json!({})),
        )?;
        self.credential
            .endpoint
            .push_str(&format!("__spki/v1/projects/{id}/"));
        println!("Workspace project {id}. Open /__spki/open/{id} in your Preview grain.");
        Ok(())
    }
    pub(crate) fn info(&self) -> Result<Info> {
        let info: Info = self.request("info", None)?;
        ensure!(
            info.protocol_version == 1 && info.deployment,
            "incompatible Preview protocol or missing deployment permission"
        );
        ensure!(
            info.host_sha256 == spki_common::HOST_SHA256
                && info.sdk_revision == spki_common::revision(b""),
            "Preview runtime/SDK differs from this CLI; update the CLI or Preview app"
        );
        ensure!(
            info.max_source_bytes > 0 && info.max_source_bytes <= 8 * 1024 * 1024,
            "invalid Preview source limit"
        );
        revision(&info.revision)?;
        Ok(info)
    }
}
pub(crate) fn revision(value: &str) -> Result<String> {
    ensure!(
        value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid Preview revision"
    );
    Ok(value.to_owned())
}
pub(crate) fn deploy(api: &Api, source: &str, expected: &mut String) -> Result<()> {
    let desired = spki_common::revision(source.as_bytes());
    let payload = Activation {
        source: source.to_owned(),
        expected_revision: expected.clone(),
    };
    ensure!(
        serde_json::to_vec(&payload)?.len() <= 9 * 1024 * 1024,
        "encoded upload exceeds Preview limit"
    );
    match api.request::<Revision>("activate", Some(&serde_json::to_value(&payload)?)) {
        Ok(value) => ensure!(
            revision(&value.revision)? == desired,
            "Preview acknowledged an unexpected revision"
        ),
        Err(error) => {
            // A lost acknowledgement may follow a successful commit. Observe before retrying.
            let live = revision(
                &api.request::<Revision>("status", None)
                    .context(DeploymentFailure::UnknownOutcome)?
                    .revision,
            )?;
            if live != desired {
                ensure!(live == *expected, DeploymentFailure::Conflict);
                return Err(error);
            }
        }
    }
    *expected = desired;
    println!("Live revision {}", &expected[..12]);
    Ok(())
}

#[derive(Debug)]
pub(crate) enum DeploymentFailure {
    Conflict,
    UnknownOutcome,
}
impl std::fmt::Display for DeploymentFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Conflict => {
                "revision conflict: another developer changed this preview; reconnect explicitly"
            }
            Self::UnknownOutcome => "activation outcome unknown; reconnect before deploying again",
        })
    }
}
impl std::error::Error for DeploymentFailure {}
