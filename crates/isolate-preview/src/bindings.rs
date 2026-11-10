//! Native bindings travel with the ES module in a versioned, single-line JSON comment.
//! This keeps build/dev/pack and recovery on the same immutable artifact. Only this
//! controller-owned header is interpreted; JavaScript is never evaluated for metadata.
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const HEADER: &str = "// spki-bindings-v1:";
pub const MAX_VALUE: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub name: String,
    pub value: Value,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Value {
    Text(String),
    Json(String),
    /// Standard base64, so arbitrary data survives JSON and the JS source artifact.
    Data(String),
    Service(String),
}
impl Binding {
    pub fn bytes(&self) -> Result<Vec<u8>> {
        Ok(match &self.value {
            Value::Data(value) => STANDARD
                .decode(value)
                .context("invalid data binding base64")?,
            Value::Text(value) | Value::Json(value) | Value::Service(value) => {
                value.as_bytes().to_vec()
            }
        })
    }
}
pub fn validate(bindings: &[Binding]) -> Result<()> {
    ensure!(
        bindings.len() <= 64,
        "at most 64 custom bindings are allowed"
    );
    let mut names = BTreeSet::new();
    for binding in bindings {
        let name = &binding.name;
        ensure!(
            name.len() <= 64
                && name
                    .bytes()
                    .enumerate()
                    .all(|(i, b)| b.is_ascii_alphabetic()
                        || b == b'_'
                        || (i > 0 && b.is_ascii_digit()))
                && !name.is_empty(),
            "invalid binding name: {name}"
        );
        ensure!(
            !name.starts_with("__")
                && !matches!(
                    name.as_str(),
                    "STORAGE" | "SANDSTORM_API" | "POWERBOX" | "constructor" | "prototype"
                ),
            "reserved binding name: {name}"
        );
        ensure!(names.insert(name), "duplicate binding name: {name}");
        let bytes = binding.bytes()?;
        ensure!(bytes.len() <= MAX_VALUE, "binding {name} exceeds 1 MiB");
        match &binding.value {
            Value::Json(value) => {
                serde_json::from_str::<serde_json::Value>(value)
                    .with_context(|| format!("invalid JSON binding {name}"))?;
            }
            Value::Service(target) => ensure!(
                target == "main",
                "service binding {name} supports only target main"
            ),
            _ => (),
        }
    }
    Ok(())
}
pub fn attach(source: String, bindings: &[Binding], limit: usize) -> Result<String> {
    validate(bindings)?;
    let result = if bindings.is_empty() {
        source
    } else {
        // U+2028/U+2029 terminate JS line comments even though JSON allows them in strings.
        let json = serde_json::to_string(bindings)?
            .replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029");
        format!("{HEADER}{json}\n{source}")
    };
    ensure!(
        result.len() <= limit,
        "worker and bindings exceed deployment limit"
    );
    Ok(result)
}
pub fn read(source: &str) -> Result<Vec<Binding>> {
    if let Some(rest) = source.strip_prefix(HEADER) {
        let (json, _) = rest.split_once('\n').context("incomplete binding header")?;
        let bindings: Vec<Binding> =
            serde_json::from_str(json).context("invalid binding header")?;
        validate(&bindings)?;
        Ok(bindings)
    } else {
        ensure!(
            !source.starts_with("// spki-bindings-"),
            "unsupported binding header version"
        );
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_bundle_includes_validated_bindings() {
        use isolate_protocol::isolate_worker_source_capnp::isolate_worker_source as wire;
        let custom = vec![
            Binding {
                name: "MESSAGE".into(),
                value: Value::Text("hi".into()),
            },
            Binding {
                name: "LOOPBACK".into(),
                value: Value::Service("main".into()),
            },
        ];
        let code = attach("export default {};".into(), &custom, 1024).unwrap();
        let packed = crate::bundle(code.as_bytes()).unwrap();
        let message =
            capnp::serialize_packed::read_message(&packed[..], Default::default()).unwrap();
        let source = message.get_root::<wire::Reader>().unwrap();
        let bindings = source.get_bindings().unwrap();
        assert_eq!(bindings.len(), 4);
        assert_eq!(bindings.get(2).get_name().unwrap(), "MESSAGE");
        assert!(
            matches!(bindings.get(2).which().unwrap(), wire::binding::Text(Ok(value)) if value==b"hi")
        );
        assert!(
            matches!(bindings.get(3).which().unwrap(), wire::binding::Service(Ok(value)) if value=="main")
        );
        let forged = code.replace("MESSAGE", "STORAGE");
        assert!(crate::bundle(forged.as_bytes()).is_err());
        assert_ne!(
            crate::controller::revision(code.as_bytes()),
            crate::controller::revision(code.replace("hi", "changed").as_bytes())
        );
    }
    #[test]
    fn artifact_roundtrip_and_authority_validation() {
        let bindings = vec![
            Binding {
                name: "MESSAGE".into(),
                value: Value::Text("hello\n\u{2028}*/".into()),
            },
            Binding {
                name: "DATA".into(),
                value: Value::Data(STANDARD.encode([0, 255, 10])),
            },
        ];
        let source = attach("export default {};".into(), &bindings, 1024).unwrap();
        let restored = read(&source).unwrap();
        assert_eq!(restored[0].bytes().unwrap(), bindings[0].bytes().unwrap());
        assert_eq!(restored[1].bytes().unwrap(), [0, 255, 10]);
        for name in [
            "STORAGE",
            "__SANDSTORM_NATIVE_CAPNP",
            "SANDSTORM_API",
            "POWERBOX",
            "__proto__",
            "constructor",
            "",
            "1X",
        ] {
            assert!(
                validate(&[Binding {
                    name: name.into(),
                    value: Value::Text("x".into())
                }])
                .is_err()
            );
        }
        assert!(validate(&[bindings[0].clone(), bindings[0].clone()]).is_err());
        for value in [
            Value::Service("another-worker".into()),
            Value::Json("{".into()),
            Value::Data("invalid!".into()),
            Value::Text("x".repeat(MAX_VALUE + 1)),
        ] {
            assert!(
                validate(&[Binding {
                    name: "X".into(),
                    value
                }])
                .is_err()
            );
        }
        assert!(attach("x".repeat(1024), &bindings, 1024).is_err());
        assert!(read("// spki-bindings-v2:[]\nx").is_err());
        assert!(read("// spki-bindings-v1:broken\nx").is_err());
    }
}
