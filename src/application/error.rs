//! Application error types, grouped by flow stage.

use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use thiserror::Error;

use crate::domain::package::InvalidPackageName;

/// Failure to identify the user who owns the Nixblend data.
#[derive(Debug, Error)]
pub enum UserError {
    #[error("could not read the local user database: {0}")]
    Database(#[source] io::Error),
    #[error("SUDO_USER is not a valid user name")]
    InvalidSudoUser,
    #[error("user '{0}' does not exist in the user database")]
    UnknownUser(String),
    #[error("UID {0} does not exist in the user database")]
    UnknownUid(u32),
    #[error("SUDO_UID does not match user '{0}' in the user database")]
    SudoUidMismatch(String),
    #[error("could not assume the identity of the original user: {0}")]
    Privileges(#[source] io::Error),
}

/// Failure to read, validate or persist the Nixblend configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("could not write {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("{path} is invalid: {message}. Fix or remove the file")]
    Invalid { path: PathBuf, message: String },
    #[error("could not serialize the configuration: {0}")]
    Serialize(String),
    #[error("no path was provided for packages.nix")]
    MissingPath,
    #[error("the path '{0}' must be absolute")]
    RelativePath(String),
    #[error("the path '{0}' does not point to a .nix file")]
    NotNixFile(String),
    #[error("no managed file is configured yet. Run 'nixblend install <package>' to set one")]
    NotConfigured,
}

/// Failure while editing the managed file. None of these failures change the file.
#[derive(Debug, Error)]
pub enum EditError {
    #[error("the configured file {0} does not exist")]
    FileMissing(PathBuf),
    #[error("{0} is not a regular file")]
    NotRegularFile(PathBuf),
    #[error("{0} is not a .nix file")]
    NotNixFile(PathBuf),
    #[error(
        "the original user cannot modify {0}, and root privileges are only used for files under /etc/nixos"
    )]
    OutsideSystemConfig(PathBuf),
    #[error("{0} is writable by any user, so it cannot be edited with root privileges")]
    UnsafePermissions(PathBuf),
    #[error("{0} has other hard links, so it cannot be used with root privileges")]
    HardLinked(PathBuf),
    #[error("'{0}' cannot be written as a package attribute")]
    InvalidAttribute(String),
    #[error("{0} is not UTF-8 encoded")]
    NotUtf8(PathBuf),
    #[error("no permission to modify {0}. Run with sudo or adjust the permissions")]
    PermissionDenied(PathBuf),
    #[error("I/O failure on {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("the file cannot be changed safely: {0}")]
    UnsupportedStructure(String),
    #[error("the current file is not valid Nix, so nothing was changed: {0}")]
    InvalidCurrentNix(String),
    #[error("the modified Nix expression is invalid; the file was not changed: {0}")]
    InvalidNix(String),
    #[error("could not run the Nix validation: {0}")]
    ValidationFailed(String),
    #[error("{0} changed during the operation; run the command again")]
    ChangedOnDisk(PathBuf),
}

impl EditError {
    /// Converts an I/O error, singling out missing permissions.
    pub fn from_io(path: PathBuf, source: io::Error) -> Self {
        match source.kind() {
            io::ErrorKind::PermissionDenied => Self::PermissionDenied(path),
            io::ErrorKind::NotFound => Self::FileMissing(path),
            _ => Self::Io { path, source },
        }
    }
}

/// The search itself failed, so nothing is known about the package.
#[derive(Debug, Error)]
pub enum SearchError {
    #[error("nix was not found")]
    ToolUnavailable,
    #[error("could not run nix: {0}")]
    Spawn(#[source] io::Error),
    #[error("{0}")]
    Failed(String),
    #[error("nix did not answer within {0:?}")]
    TimedOut(Duration),
    #[error("unexpected response from nix: {0}")]
    InvalidResponse(String),
    #[error("the flake input '{0}' was not found in the system flake.lock")]
    UnknownInput(String),
    #[error("this source cannot be searched")]
    UnsearchableSource,
}

/// Rebuild failure. An edit that was already confirmed stays applied.
#[derive(Debug, Error)]
pub enum RebuildError {
    #[error("could not start {program}: {source}")]
    Spawn { program: String, source: io::Error },
    #[error("the command ended with {0}")]
    Failed(ExitStatus),
    #[error("{0}")]
    UntrustedTool(String),
}

/// Top-level error, tagged with the stage where it happened.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("user error: {0}")]
    User(#[from] UserError),
    #[error("config error: {0}")]
    Config(#[from] ConfigError),
    #[error(
        "invalid package name: '{0}'. Use a nixpkgs attribute, such as 'git' or 'kdePackages.kate'"
    )]
    InvalidPackage(String),
    #[error(
        "validation error: could not validate the package, and nothing was changed. The package may still exist: {0}"
    )]
    Search(#[from] SearchError),
    #[error("search error: {0}")]
    SearchFailed(SearchError),
    #[error("the search term is empty")]
    EmptySearchTerm,
    #[error("package '{0}' was not found in nixpkgs. Nothing was changed")]
    PackageNotFound(String),
    #[error(
        "'{0}' is not an exact attribute, and a full search was not run. Nothing was changed. Try 'nixblend search {0}'"
    )]
    NotExactAttribute(String),
    #[error("package '{package}' is not declared in {path}. Nothing was changed{hint}")]
    NotDeclared {
        package: String,
        path: PathBuf,
        hint: String,
    },
    #[error(
        "'{package}' matches several declarations in {path}, so nothing was changed: {matches}. Name the one you mean"
    )]
    AmbiguousDeclaration {
        package: String,
        path: PathBuf,
        matches: String,
    },
    #[error("edit error: {0}")]
    Edit(#[from] EditError),
    #[error("rebuild error: {0}")]
    Rebuild(#[from] RebuildError),
    #[error("could not read the active generation: {0}")]
    Generation(String),
    #[error("terminal error: {0}")]
    Terminal(#[from] io::Error),
}

impl From<InvalidPackageName> for AppError {
    fn from(error: InvalidPackageName) -> Self {
        Self::InvalidPackage(error.0)
    }
}
