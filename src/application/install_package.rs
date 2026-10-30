//! Use case: declare a package in the managed file.

use super::error::AppError;
use super::managed::{self, Managed};
use super::ports::{Choice, Notice, PackageCatalog, Question, Services, SyntaxCheck};
use super::rebuild_step;
use crate::domain::package::{Package, PackageIdentity, PackageName};
use crate::domain::resolution::{
    InstallCheck, PackageRequest, Resolution, check_install, near_names, resolve_results,
};

/// Options shown at once when choosing among similar packages.
const CHOICE_PAGE: usize = 20;

/// Validates a package and adds its declaration.
pub struct InstallPackage<'a> {
    pub services: Services<'a>,
}

impl InstallPackage<'_> {
    /// Runs the use case for the name typed by the user.
    pub fn execute(&self, name: &str, no_rebuild: bool) -> Result<(), AppError> {
        let services = self.services;
        let name = PackageName::parse(name)?;
        let Managed { mut file, declared } = managed::open_for_edit(services)?;

        // Checked before searching: a duplicate needs no search at all.
        let request = PackageRequest::resolve(&name, &declared.aliases);
        if let InstallCheck::AlreadyDeclared(declaration) =
            check_install(&declared.declarations, &request.identity)
        {
            services.ui.notify(&Notice::AlreadyDeclared {
                requested: request.identity.qualified(),
                declaration: declaration.text.clone(),
                path: file.path().to_owned(),
            })?;
            return Ok(());
        }
        managed::check_current(services, file.as_ref())?;

        let Some(package) = self.validate(&request.identity)? else {
            services.ui.notify(&Notice::NothingChanged)?;
            return Ok(());
        };
        let identity = package.identity;

        // The validated attribute may differ from the name that was typed.
        match check_install(&declared.declarations, &identity) {
            InstallCheck::NotDeclared => {}
            InstallCheck::AlreadyDeclared(declaration) => {
                services.ui.notify(&Notice::AlreadyDeclared {
                    requested: identity.qualified(),
                    declaration: declaration.text.clone(),
                    path: file.path().to_owned(),
                })?;
                return Ok(());
            }
            InstallCheck::DeclaredElsewhere(others) => {
                services.ui.notify(&Notice::DeclaredElsewhere {
                    requested: identity.qualified(),
                    declarations: others.iter().map(|other| other.text.clone()).collect(),
                })?;
                let question = Question::AddAlongside {
                    package: identity.qualified(),
                    source: identity.source.label().to_owned(),
                };
                if !services.ui.confirm(&question)? {
                    services.ui.notify(&Notice::NothingChanged)?;
                    return Ok(());
                }
            }
        }

        let new_content = services.document.add(file.content(), &identity)?;
        // Validated before asking, so that a confirmation is never wasted.
        let check = file.check_candidate(&new_content)?;
        services.ui.notify(&Notice::Diff {
            path: file.path().to_owned(),
            old: file.content().to_owned(),
            new: new_content.clone(),
        })?;
        if check == SyntaxCheck::ToolUnavailable {
            services.ui.notify(&Notice::SyntaxNotChecked)?;
        }
        if !services.ui.confirm(&Question::ApplyEdit)? {
            services.ui.notify(&Notice::EditCancelled)?;
            return Ok(());
        }

        let original = file.content().to_owned();
        file.backup()?;
        file.replace(&new_content)?;
        services.ui.notify(&Notice::PackageAdded {
            package: identity.qualified(),
            path: file.path().to_owned(),
        })?;
        rebuild_step::run_after_edit(services, file.as_mut(), &original, no_rebuild)
    }

    /// Finds the package to install, or `None` if the user gave up. A package that
    /// cannot be confirmed to exist is never returned.
    fn validate(&self, identity: &PackageIdentity) -> Result<Option<Package>, AppError> {
        let services = self.services;
        let catalog = services.catalogs.catalog(&identity.source)?;
        if !catalog.is_pinned() {
            services.ui.notify(&Notice::RevisionNotPinned)?;
        }

        services
            .ui
            .notify(&Notice::Checking(identity.qualified()))?;
        let package = match catalog.exact(&identity.attr)? {
            Some(package) => package,
            None => match self.find_similar(catalog.as_ref(), identity)? {
                Some(package) => package,
                None => return Ok(None),
            },
        };
        services.ui.notify(&Notice::PackageFound(package.clone()))?;

        if !package.blockers().is_empty()
            && !services
                .ui
                .confirm(&Question::AddUnavailable(package.clone()))?
        {
            return Ok(None);
        }
        Ok(Some(package))
    }

    /// Looks for what the user may have meant. Typos are answered from the attribute
    /// names, which is cheap; the slow full search only runs if the user agrees.
    fn find_similar(
        &self,
        catalog: &dyn PackageCatalog,
        identity: &PackageIdentity,
    ) -> Result<Option<Package>, AppError> {
        let services = self.services;
        let attr = &identity.attr;
        let names = catalog.names()?;
        let near = near_names(&names, attr, CHOICE_PAGE);
        if !near.is_empty() {
            let packages = catalog.metadata(&near)?;
            let candidates: Vec<Package> = near
                .iter()
                .filter_map(|name| packages.get(name).cloned())
                .collect();
            if !candidates.is_empty() {
                return self.choose(identity, candidates);
            }
        }

        services
            .ui
            .notify(&Notice::NoCloseMatch(identity.qualified()))?;
        if !services
            .ui
            .confirm(&Question::RunFullSearch(identity.qualified()))?
        {
            return Err(AppError::NotExactAttribute(identity.qualified()));
        }
        match resolve_results(catalog.search(attr)?, attr) {
            Resolution::Found(package) => Ok(Some(package)),
            Resolution::NotFound => Err(AppError::PackageNotFound(identity.qualified())),
            Resolution::Candidates(candidates) => self.choose(identity, candidates),
        }
    }

    /// Offers `candidates` a page at a time. Attributes that could not be written to
    /// a Nix file are left out.
    fn choose(
        &self,
        identity: &PackageIdentity,
        mut candidates: Vec<Package>,
    ) -> Result<Option<Package>, AppError> {
        candidates.retain(|package| PackageName::parse(&package.identity.attr).is_ok());
        if candidates.is_empty() {
            return Err(AppError::PackageNotFound(identity.qualified()));
        }

        let mut shown = 0;
        loop {
            let end = candidates.len().min(shown + CHOICE_PAGE);
            let choice = self.services.ui.choose_package(
                &identity.qualified(),
                &candidates[shown..end],
                shown,
                end < candidates.len(),
            )?;
            shown = end;
            match choice {
                Choice::Index(index) if index < shown => {
                    let chosen = candidates.swap_remove(index);
                    return Ok(Some(with_source_of(identity, chosen)));
                }
                Choice::More => {}
                Choice::Index(_) | Choice::Cancel => return Ok(None),
            }
        }
    }
}

/// Candidates come from the catalog of the requested source.
fn with_source_of(requested: &PackageIdentity, mut package: Package) -> Package {
    package.identity.source = requested.source.clone();
    package
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::error::{EditError, SearchError};
    use crate::application::test_support::{Fixture, package};

    const LIST: &str = "{ pkgs, ... }: with pkgs; [ git ]";
    const WITH_CHANNEL: &str = "{ pkgs, inputs, ... }:\nlet unstable = import inputs.nixpkgs-unstable { };\nin with pkgs; [ unstable.vscode ]";

    fn run(fixture: &Fixture, name: &str) -> Result<(), AppError> {
        InstallPackage {
            services: fixture.services(),
        }
        .execute(name, false)
    }

    #[test]
    fn adds_a_validated_package_after_confirmation() {
        let fixture = Fixture::new(LIST)
            .with_package("foo")
            .answering(&[true, false]);
        run(&fixture, "foo").unwrap();
        assert!(fixture.content().contains("foo"));
        assert!(fixture.backed_up());
        assert!(fixture.notices().contains(&Notice::RebuildSkipped));
    }

    #[test]
    fn refusing_the_diff_changes_nothing() {
        let fixture = Fixture::new(LIST).with_package("foo").answering(&[false]);
        run(&fixture, "foo").unwrap();
        assert_eq!(fixture.content(), LIST);
        assert!(!fixture.backed_up());
        assert_eq!(fixture.rebuilds(), 0);
    }

    #[test]
    fn duplicate_needs_no_catalog() {
        let fixture = Fixture::new(LIST);
        run(&fixture, "git").unwrap();
        assert_eq!(fixture.catalog_calls(), 0);
        assert_eq!(fixture.content(), LIST);
    }

    #[test]
    fn same_name_from_another_source_needs_a_decision() {
        let fixture = Fixture::new(WITH_CHANNEL)
            .with_package("vscode")
            .answering(&[false]);
        run(&fixture, "vscode").unwrap();
        assert_eq!(fixture.content(), WITH_CHANNEL);
        assert!(fixture.notices().iter().any(|notice| matches!(
            notice,
            Notice::DeclaredElsewhere { declarations, .. } if declarations == &["unstable.vscode"]
        )));

        let fixture = Fixture::new(WITH_CHANNEL)
            .with_package("vscode")
            .answering(&[true, true, false]);
        run(&fixture, "vscode").unwrap();
        assert!(fixture.content().contains("unstable.vscode vscode"));
    }

    #[test]
    fn missing_package_is_not_added() {
        let fixture = Fixture::new(LIST).answering(&[true]);
        let error = run(&fixture, "nope").unwrap_err();
        assert!(matches!(error, AppError::PackageNotFound(_)));
        assert_eq!(fixture.content(), LIST);
    }

    #[test]
    fn full_search_needs_consent() {
        let fixture = Fixture::new(LIST).answering(&[false]);
        let error = run(&fixture, "nope").unwrap_err();
        assert!(matches!(error, AppError::NotExactAttribute(_)));
        assert_eq!(fixture.searches(), 0);
    }

    #[test]
    fn typos_are_answered_without_a_full_search() {
        let fixture = Fixture::new(LIST)
            .with_package("firefox")
            .choosing(Choice::Index(0))
            .answering(&[true, false]);
        run(&fixture, "firefx").unwrap();
        assert!(fixture.content().contains("firefox"));
        assert_eq!(fixture.searches(), 0);
    }

    #[test]
    fn a_failed_search_is_not_a_missing_package() {
        let fixture = Fixture::new(LIST).failing_catalog();
        let error = run(&fixture, "foo").unwrap_err();
        assert!(matches!(error, AppError::Search(SearchError::Failed(_))));
        assert_eq!(fixture.content(), LIST);
    }

    #[test]
    fn package_the_configuration_refuses_needs_a_decision() {
        let mut blocked = package("foo");
        blocked.metadata.available = Some(false);
        blocked.metadata.unfree = Some(true);
        let fixture = Fixture::new(LIST).with(blocked).answering(&[false]);
        run(&fixture, "foo").unwrap();
        assert_eq!(fixture.content(), LIST);
    }

    #[test]
    fn broken_current_file_is_refused() {
        let fixture = Fixture::new(LIST)
            .with_package("foo")
            .with_invalid_current();
        let error = run(&fixture, "foo").unwrap_err();
        assert!(matches!(
            error,
            AppError::Edit(EditError::InvalidCurrentNix(_))
        ));
    }

    #[test]
    fn invalid_result_is_refused_before_asking() {
        let fixture = Fixture::new(LIST)
            .with_package("foo")
            .with_invalid_candidate();
        let error = run(&fixture, "foo").unwrap_err();
        assert!(matches!(error, AppError::Edit(EditError::InvalidNix(_))));
        assert_eq!(fixture.content(), LIST);
        assert_eq!(fixture.confirmations(), 0);
    }

    #[test]
    fn failed_rebuild_can_undo_the_edit() {
        let fixture = Fixture::new(LIST)
            .with_package("foo")
            .with_failing_rebuild()
            .answering(&[true, true, true]);
        let error = run(&fixture, "foo").unwrap_err();
        assert!(matches!(error, AppError::Rebuild(_)));
        assert_eq!(fixture.content(), LIST);
        assert!(fixture.notices().contains(&Notice::EditUndone));
    }

    #[test]
    fn failed_rebuild_keeps_the_edit_by_default() {
        let fixture = Fixture::new(LIST)
            .with_package("foo")
            .with_failing_rebuild()
            .answering(&[true, true, false]);
        assert!(run(&fixture, "foo").is_err());
        assert!(fixture.content().contains("foo"));
    }

    #[test]
    fn no_rebuild_option_does_not_ask() {
        let fixture = Fixture::new(LIST).with_package("foo").answering(&[true]);
        InstallPackage {
            services: fixture.services(),
        }
        .execute("foo", true)
        .unwrap();
        assert_eq!(fixture.rebuilds(), 0);
        assert_eq!(fixture.confirmations(), 1);
    }

    #[test]
    fn invalid_name_stops_first() {
        let fixture = Fixture::new(LIST);
        assert!(matches!(
            run(&fixture, "foo;bar"),
            Err(AppError::InvalidPackage(_))
        ));
    }
}
