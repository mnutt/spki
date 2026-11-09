//! Native SPK framing and signing, adapted from sandstorm-marketplace's package.rs/spk.rs.
//! See licenses/Marketplace-LICENSE. Marketplace policy/PGP certification is not implemented here.
use crate::{ids, package_capnp as p};
use anyhow::{Context, Result, bail, ensure};
use capnp::{
    message::{Builder, ReaderOptions},
    serialize,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha512};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Seek, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
pub const MAGIC: &[u8; 8] = b"\x8f\xc6\xcd\xef\x45\x1a\xea\x96";
const LIMIT: u64 = 1 << 30;
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
fn files(
    path: &Path,
    mut list: capnp::struct_list::Builder<'_, p::archive::file::Owned>,
    size: &mut u64,
) -> Result<()> {
    let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for (index, entry) in entries.iter().enumerate() {
        let name = entry.file_name();
        let name = name.to_str().context("non-UTF8 filename")?;
        valid_name(name)?;
        let mut file = list.reborrow().get(index as u32);
        file.set_name(name);
        // Fixed timestamps and sorted paths make identical staging trees deterministic.
        file.set_last_modification_time_ns(0);
        let kind = entry.file_type()?;
        if kind.is_dir() {
            let count = fs::read_dir(entry.path())?.count();
            files(&entry.path(), file.init_directory(count.try_into()?), size)?;
        } else {
            ensure!(
                kind.is_file(),
                "special files and symlinks are not accepted in package staging"
            );
            let metadata = entry.metadata()?;
            *size += metadata.len();
            ensure!(*size <= LIMIT, "archive too large");
            let data = fs::read(entry.path())?;
            if metadata.permissions().mode() & 0o111 != 0 {
                file.set_executable(&data);
            } else {
                file.set_regular(&data);
            }
        }
    }
    Ok(())
}
pub fn write(stage: &Path, signing: &SigningKey, output: &Path) -> Result<()> {
    let mut archive = Builder::new_default();
    files(
        stage,
        archive
            .init_root::<p::archive::Builder>()
            .init_files(fs::read_dir(stage)?.count().try_into()?),
        &mut 0,
    )?;
    let bytes = serialize::write_message_to_words(&archive);
    ensure!(bytes.len() as u64 <= LIMIT, "archive too large");
    let hash = Sha512::digest(&bytes);
    let mut signed = signing.sign(&hash).to_bytes().to_vec();
    signed.extend_from_slice(&hash);
    let mut signature = Builder::new_default();
    let mut value = signature.init_root::<p::signature::Builder>();
    value.set_public_key(signing.verifying_key().as_bytes());
    value.set_signature(&signed);
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(MAGIC)?;
    {
        let mut options = lzma_rust2::XzOptions::default();
        options.set_block_size(std::num::NonZeroU64::new(
            u64::from(options.lzma_options.dict_size) * 3,
        ));
        let mut encoder = lzma_rust2::XzWriterMt::new(file.as_file_mut(), options, 2)?;
        serialize::write_message(&mut encoder, &signature)?;
        encoder.write_all(&bytes)?;
        encoder.finish()?;
    }
    file.as_file().sync_all()?;
    let verified = verify(file.path())?;
    ensure!(
        verified["appId"] == ids::app_id(signing.verifying_key().as_bytes()),
        "package identity verification failed"
    );
    // A pack operation never overwrites an existing release artifact.
    file.persist_noclobber(output)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
fn valid_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0']),
        "invalid archive path component"
    );
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
        let name = file.get_name()?.to_str()?;
        valid_name(name)?;
        ensure!(names.insert(name), "duplicate archive filename");
        match file.which()? {
            p::archive::file::Directory(children) => validate_files(children?, depth + 1, count)?,
            p::archive::file::Regular(data) | p::archive::file::Executable(data) => {
                let _ = data?;
            }
            p::archive::file::Symlink(target) => {
                let target = target?.to_str()?;
                ensure!(!target.contains('\0'), "invalid symlink target");
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
fn read_archive(
    input: &mut impl Read,
    size: u64,
) -> Result<capnp::message::Reader<serialize::OwnedSegments>> {
    let mut word = [0; 4];
    input.read_exact(&mut word)?;
    let count = u64::from(u32::from_le_bytes(word)) + 1;
    ensure!(count <= 65536, "too many archive segments");
    let table = (count + 2) / 2 * 8;
    ensure!(table <= size, "truncated archive framing");
    let mut lengths = serialize::SegmentLengthsBuilder::with_capacity(count as usize);
    for _ in 0..count {
        input.read_exact(&mut word)?;
        lengths.try_push_segment(u32::from_le_bytes(word) as usize)?;
        ensure!(
            lengths.total_words() as u64 <= (size - table) / 8,
            "invalid archive segment lengths"
        );
    }
    if count % 2 == 0 {
        input.read_exact(&mut word)?;
    }
    ensure!(
        lengths.total_words() as u64 * 8 == size - table,
        "trailing or truncated archive data"
    );
    let mut segments = lengths.into_owned_segments();
    input.read_exact(&mut segments)?;
    Ok(capnp::message::Reader::new(
        segments,
        limits(LIMIT as usize / 8),
    ))
}
pub fn verify(path: &Path) -> Result<serde_json::Value> {
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
    let mut archive = tempfile::tempfile()?;
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
    archive.rewind()?;
    let message = read_archive(&mut archive, size)?;
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
