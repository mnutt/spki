//! Disk-backed serialization of the fixed Sandstorm Archive schema.
//!
//! One Cap'n Proto segment is emitted with seek-patched pointers. File bodies go
//! directly from source files to the spool; the generated reader remains the
//! authority for validation. See tests comparing this layout with generated code.
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

pub(crate) const LIMIT: u64 = 1 << 30;
const MAX_FILES: usize = 100_000;
const MAX_DEPTH: usize = 64;

pub(crate) fn valid_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 255
            && name != "."
            && name != ".."
            && !name.contains(['/', '\\', '\0']),
        "invalid archive path component"
    );
    Ok(())
}
struct Writer {
    file: File,
    end: u64,
    count: usize,
}
impl Writer {
    fn reserve(&mut self, bytes: u64) -> Result<u64> {
        let start = self.end;
        self.end = self
            .end
            .checked_add(bytes)
            .context("archive size overflow")?;
        ensure!(self.end <= LIMIT, "archive too large");
        self.file.set_len(self.end)?;
        Ok(start)
    }
    fn word(&mut self, offset: u64, word: u64) -> Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(&word.to_le_bytes())?;
        Ok(())
    }
    fn list_pointer(&mut self, slot: u64, target: u64, kind: u64, count: u64) -> Result<()> {
        ensure!(count < 1 << 29, "archive list too large");
        let relative = (target as i64 - slot as i64 - 8) / 8;
        ensure!(
            (-(1 << 29)..(1 << 29)).contains(&relative),
            "archive pointer out of range"
        );
        self.word(
            slot,
            1 | ((relative as u64 & 0x3fff_ffff) << 2) | (kind << 32) | (count << 35),
        )
    }
    fn text(&mut self, slot: u64, value: &str) -> Result<()> {
        let length = value.len() as u64 + 1;
        let start = self.reserve(length.div_ceil(8) * 8)?;
        self.list_pointer(slot, start, 2, length)?;
        self.file.seek(SeekFrom::Start(start))?;
        self.file.write_all(value.as_bytes())?; // The terminator and padding are zero-filled.
        Ok(())
    }
    fn directory(&mut self, directory: &Path, slot: u64, depth: usize) -> Result<()> {
        ensure!(depth < MAX_DEPTH, "archive nesting too deep");
        let mut entries = Vec::new();
        for entry in fs::read_dir(directory)? {
            self.count += 1;
            ensure!(self.count <= MAX_FILES, "archive has too many files");
            entries.push(entry?);
        }
        entries.sort_by_key(|entry| entry.file_name());
        // Archive.File has two data words (union + timestamp) and two pointers
        // (name + union payload). Inline-composite lists begin with a struct tag.
        let count = entries.len() as u64;
        let start = self.reserve(8 + count * 32)?;
        self.list_pointer(slot, start, 7, count * 4)?;
        self.word(start, (count << 2) | (2 << 32) | (2 << 48))?;
        for (index, entry) in entries.into_iter().enumerate() {
            let name = entry.file_name();
            let name = name.to_str().context("non-UTF8 filename")?;
            valid_name(name)?;
            let base = start + 8 + index as u64 * 32;
            self.text(base + 16, name)?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                self.word(base, 3)?;
                self.directory(&entry.path(), base + 24, depth + 1)?;
            } else {
                ensure!(
                    kind.is_file(),
                    "special files and symlinks are not accepted in package staging"
                );
                let mut source = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(entry.path())?;
                let metadata = source.metadata()?;
                ensure!(metadata.is_file(), "archive source must be a regular file");
                let length = metadata.len();
                ensure!(
                    length < 1 << 29,
                    "archive file exceeds Cap'n Proto Data limit"
                );
                let data = self.reserve(length.div_ceil(8) * 8)?;
                self.word(base, u64::from(metadata.permissions().mode() & 0o111 != 0))?;
                self.list_pointer(base + 24, data, 2, length)?;
                self.file.seek(SeekFrom::Start(data))?;
                ensure!(
                    copy(&mut (&mut source).take(length), &mut self.file)? == length,
                    "file shrank during packaging"
                );
                ensure!(source.read(&mut [0; 1])? == 0, "file grew during packaging");
            }
        }
        Ok(())
    }
}
/// A fixed buffer avoids platform-specific copy implementations allocating large chunks.
pub(crate) fn copy(source: &mut impl Read, output: &mut impl Write) -> std::io::Result<u64> {
    let mut buffer = [0; 64 * 1024];
    let mut length = 0;
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            return Ok(length);
        }
        output.write_all(&buffer[..count])?;
        length += count as u64;
    }
}
pub(crate) fn spool(root: &Path, temporary: &Path) -> Result<File> {
    let mut writer = Writer {
        file: tempfile::tempfile_in(temporary)?,
        end: 0,
        count: 0,
    };
    writer.reserve(24)?; // segment table, root pointer, Archive.files pointer
    writer.word(8, 1 << 48)?; // Archive: zero data words, one pointer
    writer.directory(root, 16, 0)?;
    writer.word(0, ((writer.end - 8) / 8) << 32)?; // one segment
    writer.file.rewind()?;
    Ok(writer.file)
}
/// The file is a private, unlinked tempfile owned exclusively by this module's
/// caller. Consuming it prevents further writes or truncation through that handle.
#[allow(unsafe_code)]
pub(crate) fn freeze(file: File) -> Result<memmap2::Mmap> {
    ensure!(file.metadata()?.len() > 0, "empty archive");
    // SAFETY: all callers pass a private tempfile after completing writes. No
    // clones or writable mappings exist. Closing the handle leaves the mapping valid.
    let mapping = unsafe { memmap2::MmapOptions::new().map(&file)? };
    mapping.advise(memmap2::Advice::Random)?;
    Ok(mapping)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_capnp as p;
    #[test]
    fn disk_layout_round_trips_through_generated_reader() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("tree");
        fs::create_dir_all(root.join("dir/empty"))?;
        fs::write(root.join("dir/script"), b"#!/bin/sh\nexit 0\n")?;
        fs::set_permissions(root.join("dir/script"), fs::Permissions::from_mode(0o755))?;
        fs::write(root.join("zero"), [])?;
        fs::write(root.join("unaligned"), b"123456789")?;
        let mapping = freeze(spool(&root, temporary.path())?)?;
        let mut data = mapping.as_ref();
        let message =
            capnp::serialize::read_message_from_flat_slice(&mut data, Default::default())?;
        assert!(data.is_empty());
        let files = message.get_root::<p::archive::Reader>()?.get_files()?;
        assert_eq!(files.len(), 3);
        assert_eq!(files.get(0).get_name()?, "dir");
        let p::archive::file::Directory(children) = files.get(0).which()? else {
            panic!("directory")
        };
        let children = children?;
        let p::archive::file::Directory(empty) = children.get(0).which()? else {
            panic!("empty directory")
        };
        assert!(empty?.is_empty());
        let p::archive::file::Executable(script) = children.get(1).which()? else {
            panic!("executable")
        };
        assert_eq!(script?, b"#!/bin/sh\nexit 0\n");
        let p::archive::file::Regular(bytes) = files.get(1).which()? else {
            panic!("regular")
        };
        assert_eq!(bytes?, b"123456789");
        let p::archive::file::Regular(bytes) = files.get(2).which()? else {
            panic!("empty file")
        };
        assert!(bytes?.is_empty());
        assert_eq!(files.get(1).get_last_modification_time_ns(), 0);
        let again = freeze(spool(&root, temporary.path())?)?;
        assert_eq!(&*mapping, &*again);
        std::os::unix::fs::symlink("zero", root.join("link"))?;
        assert!(spool(&root, temporary.path()).is_err());
        Ok(())
    }
}
