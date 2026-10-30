//! In-memory implementations of the ports, for testing the use cases.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::rc::Rc;

use std::os::unix::process::ExitStatusExt;

use super::error::{AppError, ConfigError, EditError, RebuildError, SearchError};
use super::ports::{
    ActiveGeneration, CatalogProvider, Choice, Config, ConfigStore, FileMode, Interaction,
    ManagedFile, ManagedFileStore, Notice, PackageCatalog, Question, Services, SyntaxCheck,
    SystemRebuilder,
};
use crate::domain::package::{Package, PackageIdentity, PackageMetadata, PackageSource};
use crate::infrastructure::nix_file::NixPackageDocument;

/// A package with basic metadata.
pub fn package(attr: &str) -> Package {
    Package {
        identity: PackageIdentity::nixpkgs(attr),
        metadata: PackageMetadata {
            pname: Some(attr.to_owned()),
            version: Some("1.0".to_owned()),
            description: Some(format!("The {attr} tool")),
            ..PackageMetadata::default()
        },
    }
}

#[derive(Default)]
struct FileState {
    content: String,
    backed_up: bool,
    invalid_current: bool,
    invalid_candidate: bool,
}

/// A use case's whole outside world, kept in memory.
pub struct Fixture {
    file: Rc<RefCell<FileState>>,
    packages: Vec<Package>,
    has_config: bool,
    failing_catalog: bool,
    failing_rebuild: bool,
    answers: RefCell<Vec<bool>>,
    choice: Cell<Choice>,
    notices: RefCell<Vec<Notice>>,
    confirmations: Cell<u32>,
    rebuilds: Cell<u32>,
    searches: Cell<u32>,
    catalog_calls: Cell<u32>,
    store_names: HashSet<String>,
    document: NixPackageDocument,
}

impl Fixture {
    pub fn new(content: &str) -> Self {
        Self {
            file: Rc::new(RefCell::new(FileState {
                content: content.to_owned(),
                ..FileState::default()
            })),
            packages: Vec::new(),
            has_config: true,
            failing_catalog: false,
            failing_rebuild: false,
            answers: RefCell::new(Vec::new()),
            choice: Cell::new(Choice::Cancel),
            notices: RefCell::new(Vec::new()),
            confirmations: Cell::new(0),
            rebuilds: Cell::new(0),
            searches: Cell::new(0),
            catalog_calls: Cell::new(0),
            store_names: HashSet::new(),
            document: NixPackageDocument,
        }
    }

    pub fn with(mut self, package: Package) -> Self {
        self.packages.push(package);
        self
    }

    pub fn with_package(self, attr: &str) -> Self {
        self.with(package(attr))
    }

    /// Answers to confirmations, in order. Past the end, the answer is no.
    pub fn answering(self, answers: &[bool]) -> Self {
        *self.answers.borrow_mut() = answers.to_vec();
        self
    }

    pub fn choosing(self, choice: Choice) -> Self {
        self.choice.set(choice);
        self
    }

    pub fn failing_catalog(mut self) -> Self {
        self.failing_catalog = true;
        self
    }

    pub fn with_failing_rebuild(mut self) -> Self {
        self.failing_rebuild = true;
        self
    }

    pub fn with_invalid_current(self) -> Self {
        self.file.borrow_mut().invalid_current = true;
        self
    }

    pub fn with_invalid_candidate(self) -> Self {
        self.file.borrow_mut().invalid_candidate = true;
        self
    }

    pub fn without_config(mut self) -> Self {
        self.has_config = false;
        self
    }

    pub fn with_store_names(mut self, names: &[&str]) -> Self {
        self.store_names = names.iter().map(|name| (*name).to_owned()).collect();
        self
    }

    pub fn services(&self) -> Services<'_> {
        Services {
            config: self,
            files: self,
            document: &self.document,
            catalogs: self,
            rebuilder: self,
            generation: self,
            ui: self,
        }
    }

    pub fn content(&self) -> String {
        self.file.borrow().content.clone()
    }

    pub fn backed_up(&self) -> bool {
        self.file.borrow().backed_up
    }

    pub fn notices(&self) -> Vec<Notice> {
        self.notices.borrow().clone()
    }

    pub fn rebuilds(&self) -> u32 {
        self.rebuilds.get()
    }

    pub fn confirmations(&self) -> u32 {
        self.confirmations.get()
    }

    pub fn searches(&self) -> u32 {
        self.searches.get()
    }

    pub fn catalog_calls(&self) -> u32 {
        self.catalog_calls.get()
    }
}

impl ConfigStore for Fixture {
    fn load(&self) -> Result<Option<Config>, AppError> {
        Ok(self.has_config.then(|| Config {
            packages_file: PathBuf::from("/fixture/packages.nix"),
        }))
    }

    fn save(&self, _config: &Config) -> Result<(), AppError> {
        Ok(())
    }

    fn parse_path(&self, input: &str) -> Result<PathBuf, ConfigError> {
        Ok(PathBuf::from(input))
    }

    fn location(&self) -> PathBuf {
        PathBuf::from("/fixture/config.toml")
    }
}

struct FakeFile {
    state: Rc<RefCell<FileState>>,
    path: PathBuf,
    content: String,
}

impl ManagedFile for FakeFile {
    fn path(&self) -> &Path {
        &self.path
    }

    fn content(&self) -> &str {
        &self.content
    }

    fn check_current(&self) -> Result<SyntaxCheck, AppError> {
        if self.state.borrow().invalid_current {
            return Err(EditError::InvalidNix("broken".to_owned()).into());
        }
        Ok(SyntaxCheck::Valid)
    }

    fn check_candidate(&self, _new_content: &str) -> Result<SyntaxCheck, AppError> {
        if self.state.borrow().invalid_candidate {
            return Err(EditError::InvalidNix("broken".to_owned()).into());
        }
        Ok(SyntaxCheck::Valid)
    }

    fn backup(&self) -> Result<(), AppError> {
        self.state.borrow_mut().backed_up = true;
        Ok(())
    }

    fn replace(&mut self, new_content: &str) -> Result<(), AppError> {
        new_content.clone_into(&mut self.content);
        new_content.clone_into(&mut self.state.borrow_mut().content);
        Ok(())
    }
}

impl ManagedFileStore for Fixture {
    fn open(&self, path: &Path, _mode: FileMode) -> Result<Box<dyn ManagedFile>, AppError> {
        let content = self.file.borrow().content.clone();
        Ok(Box::new(FakeFile {
            state: Rc::clone(&self.file),
            path: path.to_owned(),
            content,
        }))
    }
}

struct FakeCatalog<'a> {
    fixture: &'a Fixture,
}

impl FakeCatalog<'_> {
    fn failure<T>(&self) -> Result<T, SearchError> {
        Err(SearchError::Failed("error: network down".to_owned()))
    }
}

impl PackageCatalog for FakeCatalog<'_> {
    fn exact(&self, attr: &str) -> Result<Option<Package>, SearchError> {
        if self.fixture.failing_catalog {
            return self.failure();
        }
        self.fixture
            .catalog_calls
            .set(self.fixture.catalog_calls.get() + 1);
        Ok(self
            .fixture
            .packages
            .iter()
            .find(|package| package.identity.attr == attr)
            .cloned())
    }

    fn names(&self) -> Result<Vec<String>, SearchError> {
        if self.fixture.failing_catalog {
            return self.failure();
        }
        Ok(self
            .fixture
            .packages
            .iter()
            .map(|package| package.identity.attr.clone())
            .collect())
    }

    fn search(&self, _term: &str) -> Result<Vec<Package>, SearchError> {
        if self.fixture.failing_catalog {
            return self.failure();
        }
        self.fixture.searches.set(self.fixture.searches.get() + 1);
        Ok(self.fixture.packages.clone())
    }

    fn metadata(&self, attrs: &[String]) -> Result<BTreeMap<String, Package>, SearchError> {
        if self.fixture.failing_catalog {
            return self.failure();
        }
        Ok(self
            .fixture
            .packages
            .iter()
            .filter(|package| attrs.contains(&package.identity.attr))
            .map(|package| (package.identity.attr.clone(), package.clone()))
            .collect())
    }

    fn is_pinned(&self) -> bool {
        true
    }
}

impl CatalogProvider for Fixture {
    fn catalog<'a>(
        &'a self,
        _source: &PackageSource,
    ) -> Result<Box<dyn PackageCatalog + 'a>, SearchError> {
        Ok(Box::new(FakeCatalog { fixture: self }))
    }
}

impl SystemRebuilder for Fixture {
    fn rebuild(&self) -> Result<(), RebuildError> {
        self.rebuilds.set(self.rebuilds.get() + 1);
        if self.failing_rebuild {
            return Err(RebuildError::Failed(ExitStatus::from_raw(1 << 8)));
        }
        Ok(())
    }
}

impl ActiveGeneration for Fixture {
    fn store_names(&self) -> Result<HashSet<String>, AppError> {
        Ok(self.store_names.clone())
    }
}

impl Interaction for Fixture {
    fn notify(&self, notice: &Notice) -> io::Result<()> {
        self.notices.borrow_mut().push(notice.clone());
        Ok(())
    }

    fn confirm(&self, _question: &Question) -> io::Result<bool> {
        self.confirmations.set(self.confirmations.get() + 1);
        let mut answers = self.answers.borrow_mut();
        Ok(if answers.is_empty() {
            false
        } else {
            answers.remove(0)
        })
    }

    fn ask_packages_path(&self) -> io::Result<Option<String>> {
        Ok(None)
    }

    fn choose_package(
        &self,
        _requested: &str,
        _page: &[Package],
        _first_index: usize,
        _has_more: bool,
    ) -> io::Result<Choice> {
        Ok(self.choice.get())
    }
}
