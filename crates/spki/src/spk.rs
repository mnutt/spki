//! Native SPK framing and signing, adapted from sandstorm-marketplace's package.rs/spk.rs.
//! See licenses/Marketplace-LICENSE. Marketplace policy/PGP certification is not implemented here.
use crate::{
    archive::{self, LIMIT, valid_name},
    ids, package_capnp as p,
};
use anyhow::{Context, Result, bail, ensure};
use capnp::{
    message::{Builder, ReaderOptions},
    serialize,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha512};
use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::{BufReader, Read, Seek, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
pub const MAGIC: &[u8; 8] = b"\x8f\xc6\xcd\xef\x45\x1a\xea\x96";
pub fn limits(words: usize) -> ReaderOptions {
    ReaderOptions {
        traversal_limit_in_words: Some(words),
        nesting_limit: 64,
    }
}
pub fn keyring(path: Option<PathBuf>) -> Result<PathBuf> {
    path.map(Ok).unwrap_or_else(|| {
        Ok(
            PathBuf::from(std::env::var_os("HOME").context("HOME unset; specify --keyring")?)
                .join(".sandstorm-keyring"),
        )
    })
}
pub fn keygen(path: &Path) -> Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.permissions().mode() & 0o077 == 0,
        "keyring must be a regular file with private permissions (0600)"
    );
    file.lock().context("could not lock keyring exclusively")?;
    // Refuse to append to a damaged keyring. No fallback or replacement key is generated.
    {
        let mut reader = BufReader::new(&file);
        while let Some(message) = serialize::try_read_message(&mut reader, limits(1024))? {
            validate_key(message.get_root::<p::key_file::Reader>()?)?;
        }
    }
    let mut seed = [0; 32];
    getrandom::fill(&mut seed).map_err(|_| anyhow::anyhow!("OS randomness unavailable"))?;
    let signing = SigningKey::from_bytes(&seed);
    let mut private = signing.to_bytes().to_vec();
    private.extend_from_slice(signing.verifying_key().as_bytes());
    let mut record = Builder::new_default();
    let mut key = record.init_root::<p::key_file::Builder>();
    key.set_public_key(signing.verifying_key().as_bytes());
    key.set_private_key(&private);
    serialize::write_message(&mut file, &record)?;
    file.sync_all()?;
    Ok(ids::app_id(signing.verifying_key().as_bytes()))
}
fn validate_key(key: p::key_file::Reader<'_>) -> Result<SigningKey> {
    let public: [u8; 32] = key
        .get_public_key()?
        .try_into()
        .context("invalid key public length")?;
    let private = key.get_private_key()?;
    ensure!(private.len() == 64, "invalid key private length");
    let signing = SigningKey::from_bytes(private[..32].try_into()?);
    ensure!(
        signing.verifying_key().as_bytes() == &public && private[32..] == public,
        "inconsistent signing key"
    );
    Ok(signing)
}
pub fn identity(path: &Path, id: &str) -> Result<SigningKey> {
    let expected = ids::parse_app_id(id)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .context("opening signing keyring")?;
    ensure!(file.metadata()?.is_file(), "keyring must be a regular file");
    file.lock_shared()
        .context("could not lock keyring for reading")?;
    let mut reader = BufReader::new(file);
    let mut found = None;
    while let Some(message) = serialize::try_read_message(&mut reader, limits(1024))? {
        let key = validate_key(message.get_root::<p::key_file::Reader>()?)?;
        if key.verifying_key().as_bytes() == &expected {
            found = Some(key);
        }
    }
    found.context("app ID not found in keyring; restore/import the existing key, or explicitly generate a new identity with spki keygen")
}
struct HashWriter(Sha512);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn write(stage: &Path, signing: &SigningKey, output: &Path) -> Result<()> {
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut archive = archive::spool(stage, parent)?;
    let mut digest = HashWriter(Sha512::new());
    archive::copy(&mut archive, &mut digest)?;
    archive.rewind()?;
    let hash = digest.0.finalize();
    let mut signed = signing.sign(&hash).to_bytes().to_vec();
    signed.extend_from_slice(&hash);
    let mut signature = Builder::new_default();
    let mut value = signature.init_root::<p::signature::Builder>();
    value.set_public_key(signing.verifying_key().as_bytes());
    value.set_signature(&signed);
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(MAGIC)?;
    {
        // One encoder with a fixed dictionary bounds compression memory independently
        // of archive size. Parallel encoders also retain multiple whole input blocks.
        let mut encoder =
            lzma_rust2::XzWriter::new(file.as_file_mut(), lzma_rust2::XzOptions::with_preset(3))?;
        serialize::write_message(&mut encoder, &signature)?;
        archive::copy(&mut archive, &mut encoder)?;
        encoder.finish()?;
    }
    // Verification uses a separate authenticated spool; release this one first.
    drop(archive);
    file.as_file().sync_all()?;
    let verified = verify_in(file.path(), parent)?;
    ensure!(
        verified["appId"] == ids::app_id(signing.verifying_key().as_bytes()),
        "package identity verification failed"
    );
    // A pack operation never overwrites an existing release artifact.
    file.persist_noclobber(output)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
fn validate_files(
    files: capnp::struct_list::Reader<'_, p::archive::file::Owned>,
    depth: usize,
    count: &mut usize,
) -> Result<()> {
    ensure!(depth < 64, "archive nesting too deep");
    let mut names = HashSet::new();
    for file in files {
        *count += 1;
        ensure!(*count <= 100_000, "archive has too many files");
        let name = file.get_name()?;
        ensure!(name.len() <= 255, "archive filename too long");
        let name = name.to_str()?;
        valid_name(name)?;
        ensure!(names.insert(name), "duplicate archive filename");
        match file.which()? {
            p::archive::file::Directory(children) => validate_files(children?, depth + 1, count)?,
            p::archive::file::Regular(data) | p::archive::file::Executable(data) => {
                let _ = data?;
            }
            p::archive::file::Symlink(target) => {
                let target = target?;
                ensure!(target.len() <= 4096, "symlink target too long");
                let target = target.to_str()?;
                ensure!(
                    target.len() <= 4096 && !target.contains('\0'),
                    "invalid symlink target"
                );
                let mut level = if target.starts_with('/') { 0 } else { depth };
                for part in target.split('/') {
                    match part {
                        "" | "." => (),
                        ".." => {
                            ensure!(level > 0, "symlink escapes package root");
                            level -= 1;
                        }
                        _ => level += 1,
                    }
                }
            }
        }
    }
    Ok(())
}
pub fn verify(path: &Path) -> Result<serde_json::Value> {
    verify_in(path, &std::env::temp_dir())
}
fn verify_in(path: &Path, temporary: &Path) -> Result<serde_json::Value> {
    let mut file = File::open(path)?;
    ensure!(file.metadata()?.len() <= LIMIT, "SPK exceeds 1 GiB limit");
    let mut magic = [0; 8];
    file.read_exact(&mut magic)?;
    ensure!(&magic == MAGIC, "invalid SPK magic");
    let mut decoder = lzma_rust2::XzReader::new_mem_limit(BufReader::new(file), true, 256 * 1024);
    let signature = serialize::read_message(&mut decoder, limits(1024))?;
    let signature = signature.get_root::<p::signature::Reader>()?;
    let public: [u8; 32] = signature
        .get_public_key()?
        .try_into()
        .context("invalid public key length")?;
    let signed = signature.get_signature()?;
    ensure!(signed.len() == 128, "invalid signature length");
    VerifyingKey::from_bytes(&public)?
        .verify_strict(&signed[64..], &Signature::from_slice(&signed[..64])?)?;
    let mut archive = tempfile::tempfile_in(temporary)?;
    let mut hash = Sha512::new();
    let mut size = 0;
    let mut buffer = [0; 65536];
    loop {
        let n = decoder.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        ensure!(size <= LIMIT, "decompressed archive too large");
        hash.update(&buffer[..n]);
        archive.write_all(&buffer[..n])?;
    }
    ensure!(
        hash.finalize().as_slice() == &signed[64..],
        "archive hash does not match signature"
    );
    drop(decoder);
    let mapping = archive::freeze(archive)?;
    let mut bytes = mapping.as_ref();
    let message = serialize::read_message_from_flat_slice(&mut bytes, limits(LIMIT as usize / 8))?;
    ensure!(bytes.is_empty(), "trailing archive data");
    let files = message.get_root::<p::archive::Reader>()?.get_files()?;
    validate_files(files, 0, &mut 0)?;
    for file in files {
        if file.get_name()?.to_str()? == "sandstorm-manifest" {
            let p::archive::file::Regular(bytes) = file.which()? else {
                bail!("manifest must be regular");
            };
            let mut bytes = bytes?;
            ensure!(
                bytes.len() <= p::manifest::SIZE_LIMIT_IN_WORDS as usize * 8,
                "manifest too large"
            );
            let manifest = serialize::read_message_from_flat_slice(
                &mut bytes,
                limits(p::manifest::SIZE_LIMIT_IN_WORDS as usize),
            )?;
            ensure!(bytes.is_empty(), "trailing manifest data");
            let manifest = manifest.get_root::<p::manifest::Reader>()?;
            return Ok(
                serde_json::json!({"appId":ids::app_id(&public),"version":manifest.get_app_version(),"title":manifest.get_app_title()?.get_default_text()?.to_str()?}),
            );
        }
    }
    bail!("archive has no manifest")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn keyrings_select_by_identity_and_reject_damage() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("keys");
        let one = keygen(&path)?;
        let two = keygen(&path)?;
        assert_ne!(one, two);
        for id in [&one, &two] {
            assert_eq!(
                ids::app_id(identity(&path, id)?.verifying_key().as_bytes()),
                *id
            );
        }
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        let bytes = fs::read(&path)?;
        fs::write(&path, &bytes[..bytes.len() - 1])?;
        assert!(identity(&path, &one).is_err());
        assert!(keygen(&path).is_err());
        assert_eq!(fs::read(&path)?.len(), bytes.len() - 1);
        Ok(())
    }
    fn signed(bytes: &[u8], path: &Path) -> Result<()> {
        let key = SigningKey::from_bytes(&[3; 32]);
        let hash = Sha512::digest(bytes);
        let mut value = key.sign(&hash).to_bytes().to_vec();
        value.extend_from_slice(&hash);
        let mut signature = Builder::new_default();
        let mut s = signature.init_root::<p::signature::Builder>();
        s.set_public_key(key.verifying_key().as_bytes());
        s.set_signature(&value);
        let mut file = File::create(path)?;
        file.write_all(MAGIC)?;
        let mut writer = lzma_rust2::XzWriter::new(file, lzma_rust2::XzOptions::default())?;
        serialize::write_message(&mut writer, &signature)?;
        writer.write_all(bytes)?;
        writer.finish()?;
        Ok(())
    }
    #[test]
    fn signed_archives_reject_ambiguous_paths_and_trailing_data() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("bad.spk");
        let mut manifest = Builder::new_default();
        manifest
            .init_root::<p::manifest::Builder>()
            .init_app_title()
            .set_default_text("Test");
        let manifest = serialize::write_message_to_words(&manifest);
        for name in ["../escape", "..", ".", "slash/name", "sandstorm-manifest"] {
            let mut archive = Builder::new_default();
            let mut files = archive.init_root::<p::archive::Builder>().init_files(2);
            let mut file = files.reborrow().get(0);
            file.set_name("sandstorm-manifest");
            file.set_regular(&manifest);
            let mut file = files.get(1);
            file.set_name(name);
            file.set_regular(b"test");
            signed(&serialize::write_message_to_words(&archive), &path)?;
            assert!(verify(&path).is_err(), "accepted {name}");
        }
        let mut archive = Builder::new_default();
        let mut file = archive
            .init_root::<p::archive::Builder>()
            .init_files(1)
            .get(0);
        file.set_name("sandstorm-manifest");
        file.set_regular(&manifest);
        let mut bytes = serialize::write_message_to_words(&archive);
        signed(&bytes, &path)?;
        assert!(verify(&path).is_ok());
        bytes.extend_from_slice(&[0; 8]);
        signed(&bytes, &path)?;
        assert!(verify(&path).is_err());
        Ok(())
    }
    #[test]
    fn signatures_and_compression_corruption_are_rejected() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("test.spk");
        signed(b"not an archive", &path)?;
        assert!(verify(&path).is_err());
        let mut bytes = fs::read(&path)?;
        bytes[0] ^= 1;
        fs::write(&path, &bytes)?;
        assert!(verify(&path).is_err());
        bytes[0] ^= 1;
        let index = bytes.len() / 2;
        bytes[index] ^= 1;
        fs::write(&path, &bytes)?;
        assert!(verify(&path).is_err());
        assert!(ids::parse_app_id(&"z".repeat(52)).is_err()); // Noncanonical padding.
        Ok(())
    }
}
