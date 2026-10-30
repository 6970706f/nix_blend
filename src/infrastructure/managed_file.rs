//! Access to the managed file, with the least privilege that can do the job.
//!
//! While elevated, the path comes from a file the user controls, so it must not become
//! a way to read or write arbitrary files as root. The file is first opened with the
//! identity of the original user, as enforced by the kernel. Only what that user cannot
//! reach falls back to root, and only under the system configuration.
//!
//! The directory is opened once and the file is reached through it without following
//! symlinks, so that swapping a path after these checks has no effect.

use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::atomic::{Directory, FileAttributes, StagedFile};
use super::nix_validator;
use super::user::{self, UserIdentity};
use crate::application::error::{AppError, EditError};
use crate::application::ports::{FileMode, ManagedFile, ManagedFileStore, SyntaxCheck};

const NIX_EXTENSION: &str = ".nix";
const BACKUP_SUFFIX: &str = ".nixblend-backup";

/// Directory of the system configuration. While elevated, this is the only place where
/// a file the original user cannot reach may be used.
const SYSTEM_CONFIG_DIR: &str = "/etc/nixos";

/// Identity used to touch the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    /// The process's own identity.
    Direct,
    /// The process is elevated, but the original user can do it alone.
    AsOriginalUser,
}

/// Opens managed files for one invocation.
#[derive(Debug, Clone)]
pub struct NixFileStore {
    pub user: UserIdentity,
    pub is_root: bool,
}

impl ManagedFileStore for NixFileStore {
    fn open(&self, path: &Path, mode: FileMode) -> Result<Box<dyn ManagedFile>, AppError> {
        Ok(Box::new(self.open_file(path, mode)?))
    }
}

impl NixFileStore {
    fn open_file(&self, path: &Path, mode: FileMode) -> Result<NixManagedFile, AppError> {
        if !self.is_root || self.user.uid == 0 {
            let opened = read_file(path, mode, None)?;
            return Ok(self.managed(opened, Access::Direct));
        }

        let attempt = {
            let _guard = user::drop_privileges(&self.user)?;
            read_file(path, mode, None)
        };
        match attempt {
            Ok(opened) => Ok(self.managed(opened, Access::AsOriginalUser)),
            Err(EditError::PermissionDenied(_)) => {
                let opened = read_file(path, mode, Some(Path::new(SYSTEM_CONFIG_DIR)))?;
                Ok(self.managed(opened, Access::Direct))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn managed(&self, opened: OpenedFile, access: Access) -> NixManagedFile {
        NixManagedFile {
            opened,
            access,
            user: self.user.clone(),
            is_root: self.is_root,
        }
    }
}

/// What was read from the file.
#[derive(Debug)]
struct OpenedFile {
    dir: Directory,
    name: String,
    path: PathBuf,
    content: String,
    attributes: FileAttributes,
    /// Device and inode, to detect the file being replaced.
    identity: (u64, u64),
}

/// Reads the file at `path`. With `root_scope`, the process is using privileges the
/// original user lacks, so the file must really be inside that directory and must not
/// be something any user could have written.
fn read_file(
    path: &Path,
    mode: FileMode,
    root_scope: Option<&Path>,
) -> Result<OpenedFile, EditError> {
    let io_error = |path: &Path, source| EditError::from_io(path.to_owned(), source);

    let resolved = fs::canonicalize(path).map_err(|error| io_error(path, error))?;
    let parent = resolved
        .parent()
        .ok_or_else(|| EditError::NotRegularFile(resolved.clone()))?;
    let name = resolved
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| EditError::NotRegularFile(resolved.clone()))?
        .to_owned();
    let dir = Directory::open(parent).map_err(|error| io_error(&resolved, error))?;

    if let Some(scope) = root_scope {
        let scope = fs::canonicalize(scope).unwrap_or_else(|_| scope.to_owned());
        let real = dir
            .real_path()
            .map_err(|error| io_error(&resolved, error))?;
        if !real.starts_with(&scope) {
            return Err(EditError::OutsideSystemConfig(resolved));
        }
    }

    let (bytes, metadata) = dir
        .read_file(&name)
        .map_err(|error| io_error(&resolved, error))?;
    if !metadata.is_file() {
        return Err(EditError::NotRegularFile(resolved));
    }
    // Checked on the resolved name: a `.nix` link must not reach other files.
    if !name.ends_with(NIX_EXTENSION) {
        return Err(EditError::NotNixFile(resolved));
    }
    if root_scope.is_some() && metadata.mode() & 0o002 != 0 {
        return Err(EditError::UnsafePermissions(resolved));
    }
    let content = String::from_utf8(bytes).map_err(|_| EditError::NotUtf8(resolved.clone()))?;

    let opened = OpenedFile {
        identity: identity_of(&metadata),
        attributes: attributes_of(&metadata),
        dir,
        name,
        path: resolved,
        content,
    };
    if mode == FileMode::ReadWrite {
        // Every step of a replacement except the last, so that a problem shows up now
        // and not after the user confirmed the change.
        opened.dir.remove_stale(&opened.name);
        drop(opened.stage(&opened.content)?);
    }
    Ok(opened)
}

fn attributes_of(metadata: &Metadata) -> FileAttributes {
    FileAttributes {
        mode: metadata.mode(),
        owner: Some((metadata.uid(), metadata.gid())),
    }
}

fn identity_of(metadata: &Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

impl OpenedFile {
    fn io_error(&self, source: io::Error) -> EditError {
        EditError::from_io(self.path.clone(), source)
    }

    fn stage(&self, new_content: &str) -> Result<StagedFile, EditError> {
        self.dir
            .stage(&self.name, new_content.as_bytes(), self.attributes)
            .map_err(|source| self.io_error(source))
    }

    fn backup(&self) -> Result<(), EditError> {
        let backup_name = format!(".{}{BACKUP_SUFFIX}", self.name);
        self.dir
            .stage(&backup_name, self.content.as_bytes(), self.attributes)
            .and_then(StagedFile::commit)
            .map_err(|source| self.io_error(source))
    }

    /// Replaces the file, unless another program changed or replaced it since it was
    /// read: confirmation may have taken a while.
    fn replace(&mut self, new_content: &str) -> Result<(), EditError> {
        let staged = self.stage(new_content)?;
        let changed = || EditError::ChangedOnDisk(self.path.clone());
        let (bytes, metadata) = self.dir.read_file(&self.name).map_err(|_| changed())?;
        if identity_of(&metadata) != self.identity || bytes != self.content.as_bytes() {
            return Err(changed());
        }
        staged.commit().map_err(|source| self.io_error(source))?;

        let (_, metadata) = self
            .dir
            .read_file(&self.name)
            .map_err(|source| self.io_error(source))?;
        self.identity = identity_of(&metadata);
        new_content.clone_into(&mut self.content);
        Ok(())
    }
}

/// An open managed file.
#[derive(Debug)]
pub struct NixManagedFile {
    opened: OpenedFile,
    access: Access,
    user: UserIdentity,
    is_root: bool,
}

impl NixManagedFile {
    /// Runs `operation` with the identity chosen for this file.
    fn with_access<T>(
        &self,
        operation: impl FnOnce(&OpenedFile) -> Result<T, EditError>,
    ) -> Result<T, AppError> {
        let _guard = match self.access {
            Access::Direct => None,
            Access::AsOriginalUser => Some(user::drop_privileges(&self.user)?),
        };
        Ok(operation(&self.opened)?)
    }

    fn check_content(&self, content: &str) -> Result<SyntaxCheck, AppError> {
        let (staged, reader) = self.with_access(|opened| {
            let staged = opened.stage(content)?;
            let reader = staged.reader().map_err(|source| opened.io_error(source))?;
            Ok((staged, reader))
        })?;
        // The guard is gone: switching identity for the tool requires root again. The
        // tool only reads the open file, so it never needs privileges.
        let run_as = (self.is_root && self.user.uid != 0).then_some(&self.user);
        let check = nix_validator::check_syntax(reader, self.is_root, run_as)?;
        drop(staged);
        Ok(check)
    }
}

impl ManagedFile for NixManagedFile {
    fn path(&self) -> &Path {
        &self.opened.path
    }

    fn content(&self) -> &str {
        &self.opened.content
    }

    fn check_current(&self) -> Result<SyntaxCheck, AppError> {
        self.check_content(&self.opened.content)
    }

    fn check_candidate(&self, new_content: &str) -> Result<SyntaxCheck, AppError> {
        self.check_content(new_content)
    }

    fn backup(&self) -> Result<(), AppError> {
        self.with_access(OpenedFile::backup)
    }

    fn replace(&mut self, new_content: &str) -> Result<(), AppError> {
        let _guard = match self.access {
            Access::Direct => None,
            Access::AsOriginalUser => Some(user::drop_privileges(&self.user)?),
        };
        Ok(self.opened.replace(new_content)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    const STALE: &str = ".packages.nix.nixblend-4000000000-0";

    fn open(path: &Path, mode: FileMode) -> Result<OpenedFile, EditError> {
        read_file(path, mode, None)
    }

    fn entries(directory: &Path) -> usize {
        fs::read_dir(directory).unwrap().count()
    }

    #[test]
    fn open_reports_missing_and_invalid_files() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.nix");
        assert!(matches!(
            open(&missing, FileMode::ReadOnly),
            Err(EditError::FileMissing(_))
        ));
        assert!(matches!(
            open(directory.path(), FileMode::ReadOnly),
            Err(EditError::NotRegularFile(_))
        ));
        let binary = directory.path().join("binary.nix");
        fs::write(&binary, [0xff, 0xfe]).unwrap();
        assert!(matches!(
            open(&binary, FileMode::ReadOnly),
            Err(EditError::NotUtf8(_))
        ));
    }

    #[test]
    fn open_refuses_targets_that_are_not_nix_files() {
        let directory = tempfile::tempdir().unwrap();
        let text = directory.path().join("notes.txt");
        let link = directory.path().join("link.nix");
        fs::write(&text, "x").unwrap();
        symlink(&text, &link).unwrap();
        for path in [&text, &link] {
            assert!(matches!(
                open(path, FileMode::ReadOnly),
                Err(EditError::NotNixFile(_))
            ));
        }
    }

    #[test]
    fn root_scope_follows_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let system = directory.path().join("system");
        fs::create_dir(&system).unwrap();
        let inside = system.join("a.nix");
        let outside = directory.path().join("b.nix");
        let escape = system.join("escape.nix");
        fs::write(&inside, "").unwrap();
        fs::write(&outside, "").unwrap();
        symlink(&outside, &escape).unwrap();

        assert!(read_file(&inside, FileMode::ReadOnly, Some(&system)).is_ok());
        for path in [&outside, &escape] {
            assert!(matches!(
                read_file(path, FileMode::ReadOnly, Some(&system)),
                Err(EditError::OutsideSystemConfig(_))
            ));
        }
    }

    #[test]
    fn root_scope_refuses_world_writable_files() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("a.nix");
        fs::write(&file, "").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(matches!(
            read_file(&file, FileMode::ReadOnly, Some(directory.path())),
            Err(EditError::UnsafePermissions(_))
        ));
    }

    #[test]
    fn replace_preserves_mode_and_leaves_no_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("packages.nix");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        let mut file = open(&path, FileMode::ReadWrite).unwrap();
        file.replace("new").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(file.content, "new");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o640);
        assert_eq!(entries(directory.path()), 1);
    }

    #[test]
    fn replace_can_be_undone_with_the_same_object() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("packages.nix");
        fs::write(&path, "old").unwrap();

        let mut file = open(&path, FileMode::ReadWrite).unwrap();
        file.replace("new").unwrap();
        file.replace("old").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "old");
    }

    #[test]
    fn replace_writes_through_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("real.nix");
        let link = directory.path().join("link.nix");
        fs::write(&real, "old").unwrap();
        symlink(&real, &link).unwrap();

        let mut file = open(&link, FileMode::ReadWrite).unwrap();
        file.replace("new").unwrap();

        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
    }

    #[test]
    fn concurrent_edit_is_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("packages.nix");
        fs::write(&path, "old").unwrap();

        let mut file = open(&path, FileMode::ReadWrite).unwrap();
        fs::write(&path, "edited elsewhere").unwrap();

        assert!(matches!(
            file.replace("new"),
            Err(EditError::ChangedOnDisk(_))
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "edited elsewhere");
        assert_eq!(entries(directory.path()), 1);
    }

    #[test]
    fn file_swapped_for_identical_content_is_detected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("packages.nix");
        fs::write(&path, "old").unwrap();

        let mut file = open(&path, FileMode::ReadWrite).unwrap();
        let swapped = directory.path().join("swapped");
        fs::write(&swapped, "old").unwrap();
        fs::rename(&swapped, &path).unwrap();

        assert!(matches!(
            file.replace("new"),
            Err(EditError::ChangedOnDisk(_))
        ));
    }

    #[test]
    fn file_replaced_by_a_symlink_is_detected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("packages.nix");
        let other = directory.path().join("other");
        fs::write(&path, "old").unwrap();
        fs::write(&other, "old").unwrap();

        let mut file = open(&path, FileMode::ReadWrite).unwrap();
        fs::remove_file(&path).unwrap();
        symlink(&other, &path).unwrap();

        assert!(matches!(
            file.replace("new"),
            Err(EditError::ChangedOnDisk(_))
        ));
        assert_eq!(fs::read_to_string(&other).unwrap(), "old");
    }

    #[test]
    fn deleted_file_is_detected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("packages.nix");
        fs::write(&path, "old").unwrap();

        let mut file = open(&path, FileMode::ReadWrite).unwrap();
        fs::remove_file(&path).unwrap();

        assert!(matches!(
            file.replace("new"),
            Err(EditError::ChangedOnDisk(_))
        ));
        assert!(!path.exists());
    }

    #[test]
    fn backup_keeps_the_attributes_and_is_not_seen_as_stale() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("packages.nix");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        let file = open(&path, FileMode::ReadWrite).unwrap();
        file.backup().unwrap();

        let backup = directory.path().join(".packages.nix.nixblend-backup");
        assert_eq!(fs::read_to_string(&backup).unwrap(), "old");
        assert_eq!(fs::metadata(&backup).unwrap().mode() & 0o7777, 0o640);

        open(&path, FileMode::ReadWrite).unwrap();
        assert!(backup.exists());
    }

    #[test]
    fn opening_for_edit_removes_leftovers_and_checks_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("packages.nix");
        fs::write(&path, "old").unwrap();
        let stale = directory.path().join(STALE);
        fs::write(&stale, "").unwrap();

        open(&path, FileMode::ReadWrite).unwrap();
        assert!(!stale.exists());
        assert_eq!(entries(directory.path()), 1);

        if rustix::process::geteuid().is_root() {
            eprintln!("SKIPPED permission check: running as root");
            return;
        }
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let writable = open(&path, FileMode::ReadWrite);
        let readable = open(&path, FileMode::ReadOnly);
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();

        assert!(matches!(writable, Err(EditError::PermissionDenied(_))));
        assert!(readable.is_ok());
    }
}
