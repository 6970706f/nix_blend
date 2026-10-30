//! Atomic writes: the content is prepared in a temporary file in the same directory as
//! the target and only then replaces it.
//!
//! Everything goes through a descriptor of the directory, opened once. Replacing a
//! directory in the path by a symlink after that has no effect on where files are
//! read or written.

use std::fs::{self, File, Permissions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt, fchown};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Mutex;

use rustix::fs::{AtFlags, Mode, OFlags, fsync, open, openat, renameat, unlinkat};

const MAX_NAME_ATTEMPTS: u32 = 64;
const TEMP_MARKER: &str = ".nixblend-";

/// Temporary files that exist right now, for cleanup when interrupted.
static LIVE_TEMP_FILES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Mode and, optionally, owner (`uid`, `gid`) of the final file.
#[derive(Debug, Clone, Copy)]
pub struct FileAttributes {
    pub mode: u32,
    pub owner: Option<(u32, u32)>,
}

/// An open directory.
#[derive(Debug)]
pub struct Directory {
    fd: OwnedFd,
    path: PathBuf,
}

impl Directory {
    /// Opens the directory at `path`.
    pub fn open(path: &Path) -> io::Result<Self> {
        let fd = open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Self {
            fd,
            path: path.to_owned(),
        })
    }

    /// Where the directory actually is, regardless of the path used to open it.
    pub fn real_path(&self) -> io::Result<PathBuf> {
        fs::read_link(format!("/proc/self/fd/{}", self.fd.as_raw_fd()))
    }

    /// Opens a file in the directory for reading. A symlink is refused, and a special
    /// file cannot block the process.
    pub fn open_file(&self, name: &str) -> io::Result<File> {
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        Ok(File::from(openat(&self.fd, name, flags, Mode::empty())?))
    }

    /// Reads the file `name` with its metadata. Anything but a regular file comes back
    /// empty, for the caller to judge by the metadata.
    pub fn read_file(&self, name: &str) -> io::Result<(Vec<u8>, fs::Metadata)> {
        let mut file = self.open_file(name)?;
        let metadata = file.metadata()?;
        let mut content = Vec::new();
        // Nothing is read from anything else, which could fail or never end.
        if metadata.is_file() {
            file.read_to_end(&mut content)?;
        }
        Ok((content, metadata))
    }

    /// Writes `content` to a temporary file next to `target`, with the final attributes
    /// already applied. `target` is not touched.
    pub fn stage(
        &self,
        target: &str,
        content: &[u8],
        attributes: FileAttributes,
    ) -> io::Result<StagedFile> {
        let (file, temp) = self.create_temp(target)?;
        let staged = StagedFile {
            dir: self.fd.as_fd().try_clone_to_owned()?,
            live_path: self.path.join(&temp),
            temp,
            target: target.to_owned(),
            committed: false,
        };
        LIVE_TEMP_FILES
            .lock()
            .map_err(|_| io::Error::other("poisoned lock"))?
            .push(staged.live_path.clone());
        fill(&file, content, attributes)?;
        Ok(staged)
    }

    fn create_temp(&self, target: &str) -> io::Result<(File, String)> {
        let pid = process::id();
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        for attempt in 0..MAX_NAME_ATTEMPTS {
            let temp = format!(".{target}{TEMP_MARKER}{pid}-{attempt}");
            match openat(&self.fd, temp.as_str(), flags, Mode::RUSR | Mode::WUSR) {
                Ok(fd) => return Ok((File::from(fd), temp)),
                Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create a temporary file",
        ))
    }

    /// Removes temporary files of `target` whose creating process no longer exists, as
    /// after a run that was killed. Failures are ignored: a leftover file is harmless,
    /// and this must not block an edit.
    pub fn remove_stale(&self, target: &str) {
        let prefix = format!(".{target}{TEMP_MARKER}");
        let Ok(entries) = fs::read_dir(&self.path) else {
            return;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let Some(pid) = name
                .strip_prefix(&prefix)
                .and_then(|suffix| suffix.split('-').next())
                .and_then(|pid| pid.parse::<u32>().ok())
            else {
                continue;
            };
            if !Path::new("/proc").join(pid.to_string()).exists() {
                let _ = unlinkat(&self.fd, name, AtFlags::empty());
            }
        }
    }
}

fn fill(mut file: &File, content: &[u8], attributes: FileAttributes) -> io::Result<()> {
    file.write_all(content)?;
    if let Some((uid, gid)) = attributes.owner {
        let current = file.metadata()?;
        if (current.uid(), current.gid()) != (uid, gid) {
            fchown(file, Some(uid), Some(gid))?;
        }
    }
    // The mode comes after the owner because `chown` clears the setuid and setgid bits.
    file.set_permissions(Permissions::from_mode(attributes.mode & 0o7777))?;
    file.sync_all()
}

/// Temporary file ready to replace the target. It is removed when dropped without
/// [`StagedFile::commit`].
#[derive(Debug)]
pub struct StagedFile {
    dir: OwnedFd,
    live_path: PathBuf,
    temp: String,
    target: String,
    committed: bool,
}

impl StagedFile {
    /// Opens the prepared content for reading, for validation.
    pub fn reader(&self) -> io::Result<File> {
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        Ok(File::from(openat(
            &self.dir,
            self.temp.as_str(),
            flags,
            Mode::empty(),
        )?))
    }

    /// Replaces the target atomically.
    pub fn commit(mut self) -> io::Result<()> {
        renameat(
            &self.dir,
            self.temp.as_str(),
            &self.dir,
            self.target.as_str(),
        )?;
        self.committed = true;
        // The swap is already visible at this point. Not every file system can sync
        // directories, so a failure here does not invalidate the write.
        let _ = fsync(&self.dir);
        Ok(())
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = unlinkat(&self.dir, self.temp.as_str(), AtFlags::empty());
        }
        if let Ok(mut live) = LIVE_TEMP_FILES.lock() {
            live.retain(|path| *path != self.live_path);
        }
    }
}

/// Removes the temporary files that exist right now. Used when interrupted.
pub fn remove_registered() {
    if let Ok(live) = LIVE_TEMP_FILES.lock() {
        for path in live.iter() {
            let _ = fs::remove_file(path);
        }
    }
}

/// Writes a whole file atomically. The directory must exist.
pub fn write_file(path: &Path, content: &[u8], attributes: FileAttributes) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid file name"))?;
    Directory::open(parent)?
        .stage(name, content, attributes)?
        .commit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const PRIVATE: FileAttributes = FileAttributes {
        mode: 0o640,
        owner: None,
    };

    fn entries(directory: &Path) -> usize {
        fs::read_dir(directory).unwrap().count()
    }

    #[test]
    fn staging_leaves_the_target_untouched_until_commit() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("file");
        fs::write(&target, "old").unwrap();
        let dir = Directory::open(directory.path()).unwrap();

        let staged = dir.stage("file", b"new", PRIVATE).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
        let mut read_back = String::new();
        staged
            .reader()
            .unwrap()
            .read_to_string(&mut read_back)
            .unwrap();
        assert_eq!(read_back, "new");

        staged.commit().unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(entries(directory.path()), 1);
    }

    #[test]
    fn commit_applies_the_requested_mode() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("file");

        write_file(&target, b"new", PRIVATE).unwrap();

        let mode = fs::metadata(&target).unwrap().mode();
        assert_eq!(mode & 0o7777, 0o640);
    }

    #[test]
    fn dropping_without_commit_removes_the_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("file");
        fs::write(&target, "old").unwrap();
        let dir = Directory::open(directory.path()).unwrap();

        drop(dir.stage("file", b"new", PRIVATE).unwrap());

        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
        assert_eq!(entries(directory.path()), 1);
    }

    #[test]
    fn temporary_files_are_tracked_only_while_they_exist() {
        // Cleaning up for real would hit the files of tests running in parallel. The
        // removal itself is covered by a full-flow test that interrupts the program.
        let is_tracked = |path: &Path| LIVE_TEMP_FILES.lock().unwrap().iter().any(|p| p == path);
        let directory = tempfile::tempdir().unwrap();
        let dir = Directory::open(directory.path()).unwrap();

        let staged = dir.stage("tracked", b"new", PRIVATE).unwrap();
        let live_path = staged.live_path.clone();
        assert!(live_path.exists());
        assert!(is_tracked(&live_path));

        drop(staged);
        assert!(!is_tracked(&live_path));
    }

    #[test]
    fn removes_only_stale_temporary_files_of_the_target() {
        let directory = tempfile::tempdir().unwrap();
        let path = |name: &str| directory.path().join(name);
        let live = format!(".file.nixblend-{}-0", process::id());
        for name in [
            ".file.nixblend-4000000000-0",
            live.as_str(),
            ".other.nixblend-4000000000-0",
            ".file.nixblend-x",
            ".file.nixblend-backup",
        ] {
            fs::write(path(name), "").unwrap();
        }

        Directory::open(directory.path())
            .unwrap()
            .remove_stale("file");

        assert!(!path(".file.nixblend-4000000000-0").exists());
        assert!(path(&live).exists());
        assert!(path(".other.nixblend-4000000000-0").exists());
        assert!(path(".file.nixblend-x").exists());
        assert!(path(".file.nixblend-backup").exists());
    }

    #[test]
    fn missing_directory_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("missing/file");

        let error = write_file(&target, b"new", PRIVATE).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn symlinks_are_not_followed_when_reading() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("real"), "x").unwrap();
        symlink(directory.path().join("real"), directory.path().join("link")).unwrap();
        let dir = Directory::open(directory.path()).unwrap();

        assert!(dir.open_file("real").is_ok());
        assert!(dir.open_file("link").is_err());
    }

    #[test]
    fn a_directory_swapped_after_opening_does_not_redirect_writes() {
        let base = tempfile::tempdir().unwrap();
        let original = base.path().join("original");
        let elsewhere = base.path().join("elsewhere");
        fs::create_dir(&original).unwrap();
        fs::create_dir(&elsewhere).unwrap();
        let dir = Directory::open(&original).unwrap();

        let moved = base.path().join("moved");
        fs::rename(&original, &moved).unwrap();
        symlink(&elsewhere, &original).unwrap();
        dir.stage("file", b"new", PRIVATE)
            .unwrap()
            .commit()
            .unwrap();

        assert!(moved.join("file").exists());
        assert_eq!(entries(&elsewhere), 0);
    }

    #[test]
    fn real_path_reports_where_the_directory_is() {
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("real");
        let link = base.path().join("link");
        fs::create_dir(&real).unwrap();
        symlink(&real, &link).unwrap();

        let dir = Directory::open(&link).unwrap();

        assert_eq!(dir.real_path().unwrap(), fs::canonicalize(&real).unwrap());
    }
}
