//! Interfaces the use cases need from the outside world.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use super::error::{AppError, ConfigError, EditError, RebuildError, SearchError};
use crate::domain::package::{
    DeclaredPackages, Package, PackageDeclaration, PackageIdentity, PackageSource,
};

/// Persisted user configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// File managed by Nixblend.
    pub packages_file: PathBuf,
}

/// Where the configuration lives.
pub trait ConfigStore {
    /// Loads the configuration, or `None` if there is none yet.
    fn load(&self) -> Result<Option<Config>, AppError>;
    /// Saves the configuration.
    fn save(&self, config: &Config) -> Result<(), AppError>;
    /// Validates the form of a path typed by the user.
    fn parse_path(&self, input: &str) -> Result<PathBuf, ConfigError>;
    /// Location of the configuration, for messages.
    fn location(&self) -> PathBuf;
}

/// Purpose of opening the managed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMode {
    ReadOnly,
    ReadWrite,
}

/// Outcome of a completed syntax check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxCheck {
    Valid,
    /// The tool is not available, so the syntax was not checked.
    ToolUnavailable,
}

/// Opens managed files, with the checks and privileges each one requires.
pub trait ManagedFileStore {
    /// Opens the file at `path`.
    fn open(&self, path: &Path, mode: FileMode) -> Result<Box<dyn ManagedFile>, AppError>;
}

/// An open managed file.
pub trait ManagedFile {
    /// Path of the file, with symlinks resolved.
    fn path(&self) -> &Path;
    /// Content when it was opened or last replaced.
    fn content(&self) -> &str;
    /// Checks the syntax of the current content.
    fn check_current(&self) -> Result<SyntaxCheck, AppError>;
    /// Prepares `new_content` in a temporary file and checks its syntax, without
    /// changing the file.
    fn check_candidate(&self, new_content: &str) -> Result<SyntaxCheck, AppError>;
    /// Saves a copy of the current content next to the file.
    fn backup(&self) -> Result<(), AppError>;
    /// Atomically replaces the file, refusing if it changed since it was read.
    fn replace(&mut self, new_content: &str) -> Result<(), AppError>;
}

/// Understands the structure of a package file.
pub trait PackageDocument {
    /// Finds the declarations of `source`, refusing unsupported structures.
    fn analyze(&self, source: &str) -> Result<DeclaredPackages, EditError>;
    /// Returns `source` with a declaration of `identity` added.
    fn add(&self, source: &str, identity: &PackageIdentity) -> Result<String, EditError>;
    /// Returns `source` without `declaration`, keeping everything else.
    fn remove(&self, source: &str, declaration: &PackageDeclaration) -> Result<String, EditError>;
}

/// Package data of one source.
pub trait PackageCatalog {
    /// Looks up one exact attribute. `Ok(None)` means it is not a package; a failure
    /// to find out is an error, never `None`.
    fn exact(&self, attr: &str) -> Result<Option<Package>, SearchError>;
    /// Names of the top-level attributes, which is much cheaper than a search.
    fn names(&self) -> Result<Vec<String>, SearchError>;
    /// Searches names and descriptions. This can be slow.
    fn search(&self, term: &str) -> Result<Vec<Package>, SearchError>;
    /// Complete metadata of known attributes, which may be missing from the result.
    fn metadata(&self, attrs: &[String]) -> Result<BTreeMap<String, Package>, SearchError>;
    /// Whether results are from the revision locked by the system's configuration.
    fn is_pinned(&self) -> bool;
}

/// Gives access to the catalog of each source.
pub trait CatalogProvider {
    /// Catalog of `source`.
    fn catalog<'a>(
        &'a self,
        source: &PackageSource,
    ) -> Result<Box<dyn PackageCatalog + 'a>, SearchError>;
}

/// Applies the system configuration.
pub trait SystemRebuilder {
    /// Runs the rebuild.
    fn rebuild(&self) -> Result<(), RebuildError>;
}

/// The system generation currently active.
pub trait ActiveGeneration {
    /// Names of the store paths in the active generation, without hashes.
    fn store_names(&self) -> Result<HashSet<String>, AppError>;
}

/// Something the user should know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// The path could not be used and was not saved.
    PathNotSaved,
    /// The managed file is defined in this configuration file.
    ManagedFileConfiguredIn(PathBuf),
    /// Results may come from another revision than the system's.
    RevisionNotPinned,
    Checking(String),
    /// The package exists.
    PackageFound(Package),
    /// The term looks like a typo of these attributes.
    DidYouMean(Vec<String>),
    /// Nothing close to the name exists among top-level attributes.
    NoCloseMatch(String),
    AlreadyDeclared {
        requested: String,
        declaration: String,
        path: PathBuf,
    },
    /// The package is declared with other sources only.
    DeclaredElsewhere {
        requested: String,
        declarations: Vec<String>,
    },
    SyntaxNotChecked,
    Diff {
        path: PathBuf,
        old: String,
        new: String,
    },
    EditCancelled,
    NothingChanged,
    PackageAdded {
        package: String,
        path: PathBuf,
    },
    /// This declaration will be removed.
    WillRemove {
        declaration: String,
        line: usize,
    },
    PackageRemoved {
        package: String,
        path: PathBuf,
    },
    /// The declaration will be reapplied, which is not a forced new download.
    Reinstalling {
        declaration: String,
        source: String,
    },
    RebuildSkipped,
    RebuildDone,
    RebuildFailed,
    /// The file is back to its previous content.
    EditUndone,
}

/// A decision only the user can make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    ApplyEdit,
    RunRebuild,
    /// The configuration refuses this package.
    AddUnavailable(Package),
    /// Declare the package alongside the same name from another source.
    AddAlongside {
        package: String,
        source: String,
    },
    /// Nothing close was found: search names and descriptions, which can be slow.
    RunFullSearch(String),
    /// The rebuild failed: go back to the previous content of the file.
    UndoEdit,
}

/// Answer to a choice among packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Zero-based index of the chosen option.
    Index(usize),
    More,
    Cancel,
}

/// Conversation with the user.
pub trait Interaction {
    /// Shows a notice.
    fn notify(&self, notice: &Notice) -> io::Result<()>;
    /// Asks for an explicit decision. Anything but a clear yes is a no.
    fn confirm(&self, question: &Question) -> io::Result<bool>;
    /// Asks for the path of the file to manage.
    fn ask_packages_path(&self) -> io::Result<Option<String>>;
    /// Shows `page`, numbered from `first_index`, and asks for a choice among all
    /// options shown so far.
    fn choose_package(
        &self,
        requested: &str,
        page: &[Package],
        first_index: usize,
        has_more: bool,
    ) -> io::Result<Choice>;
}

/// Reports the progress of slow operations.
pub trait ProgressReporter {
    /// The operation `label` has been running for `seconds`.
    fn update(&self, label: &str, seconds: u64);
    /// The operation ended.
    fn finish(&self);
}

/// Everything a use case may need.
#[derive(Clone, Copy)]
pub struct Services<'a> {
    pub config: &'a dyn ConfigStore,
    pub files: &'a dyn ManagedFileStore,
    pub document: &'a dyn PackageDocument,
    pub catalogs: &'a dyn CatalogProvider,
    pub rebuilder: &'a dyn SystemRebuilder,
    pub generation: &'a dyn ActiveGeneration,
    pub ui: &'a dyn Interaction,
}
