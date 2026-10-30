//! Use case: list what the managed file declares.
//!
//! The listing reflects the declarations, not imperative installations. Comparing
//! them with the active generation is a separate, slower step.

use std::collections::HashSet;
use std::path::PathBuf;

use super::error::AppError;
use super::managed;
use super::ports::Services;
use crate::domain::package::{PackageDeclaration, PackageIdentity, PackageSource};
use crate::domain::resolution::unique_declarations;

/// Whether a declaration is present in the active system generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveState {
    Active,
    NotActive,
    Unknown,
}

/// One line of the listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedPackage {
    pub name: String,
    pub source: String,
    /// Present when the comparison with the active generation was requested.
    pub active: Option<ActiveState>,
}

/// Result of the listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageList {
    pub path: PathBuf,
    pub packages: Vec<ListedPackage>,
}

/// Reads the declarations of the managed file.
pub struct ListPackages<'a> {
    pub services: Services<'a>,
}

impl ListPackages<'_> {
    /// Runs the use case.
    pub fn execute(&self, compare_active: bool) -> Result<PackageList, AppError> {
        let managed = managed::open_for_reading(self.services)?;
        let declarations = unique_declarations(&managed.declared.declarations);

        let active = if compare_active {
            Some(self.active_states(&declarations)?)
        } else {
            None
        };
        let packages = declarations
            .iter()
            .enumerate()
            .map(|(index, declaration)| ListedPackage {
                name: display_name(declaration),
                source: declaration
                    .identity
                    .as_ref()
                    .map_or(PackageSource::Unknown.label(), |identity| {
                        identity.source.label()
                    })
                    .to_owned(),
                active: active.as_ref().map(|states| states[index]),
            })
            .collect();
        Ok(PackageList {
            path: managed.file.path().to_owned(),
            packages,
        })
    }

    /// Compares by exact store path name when the system configuration can tell it,
    /// and otherwise by attribute name, which can only confirm presence: an attribute
    /// may differ from the name of its store path.
    fn active_states(
        &self,
        declarations: &[&PackageDeclaration],
    ) -> Result<Vec<ActiveState>, AppError> {
        let store_names = self.services.generation.store_names()?;
        let attrs: Vec<String> = declarations
            .iter()
            .filter_map(|declaration| declaration.identity.as_ref())
            .filter(|identity| identity.source == PackageSource::Nixpkgs)
            .map(|identity| identity.attr.clone())
            .collect();
        let metadata = self
            .services
            .catalogs
            .catalog(&PackageSource::Nixpkgs)
            .and_then(|catalog| catalog.metadata(&attrs))
            .unwrap_or_default();

        Ok(declarations
            .iter()
            .map(|declaration| {
                let Some(identity) = &declaration.identity else {
                    return ActiveState::Unknown;
                };
                let exact = (identity.source == PackageSource::Nixpkgs)
                    .then(|| metadata.get(&identity.attr))
                    .flatten()
                    .and_then(|package| package.metadata.store_name.as_deref());
                match exact {
                    Some(name) if store_names.contains(name) => ActiveState::Active,
                    Some(_) => ActiveState::NotActive,
                    None if has_store_path_named(&store_names, identity) => ActiveState::Active,
                    None => ActiveState::Unknown,
                }
            })
            .collect())
    }
}

fn has_store_path_named(store_names: &HashSet<String>, identity: &PackageIdentity) -> bool {
    let name = identity.attr.rsplit('.').next().unwrap_or(&identity.attr);
    store_names.iter().any(|store_name| {
        store_name == name
            || store_name
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    })
}

fn display_name(declaration: &PackageDeclaration) -> String {
    match &declaration.identity {
        Some(identity) => identity.attr.clone(),
        None => declaration
            .text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::error::ConfigError;
    use crate::application::test_support::{Fixture, package};

    const LIST: &str = "{ pkgs, inputs, ... }:\nlet unstable = import inputs.nixpkgs-unstable { };\nin with pkgs; [\n  firefox\n  unstable.vscode\n  git\n  git\n  inputs.tool.packages.x.default\n  (foo.override { })\n  \"text\"\n]\n";

    fn list(fixture: &Fixture, active: bool) -> PackageList {
        ListPackages {
            services: fixture.services(),
        }
        .execute(active)
        .unwrap()
    }

    fn lines(list: &PackageList) -> Vec<(&str, &str)> {
        list.packages
            .iter()
            .map(|package| (package.name.as_str(), package.source.as_str()))
            .collect()
    }

    #[test]
    fn lists_declarations_with_their_sources_once() {
        let fixture = Fixture::new(LIST);
        assert_eq!(
            lines(&list(&fixture, false)),
            [
                ("firefox", "nixpkgs"),
                ("vscode", "unstable"),
                ("git", "nixpkgs"),
                ("tool", "tool"),
                ("foo", "nixpkgs"),
                ("\"text\"", "unknown"),
            ]
        );
        assert_eq!(fixture.catalog_calls(), 0);
    }

    #[test]
    fn without_a_configuration_there_is_nothing_to_list() {
        let fixture = Fixture::new(LIST).without_config();
        let error = ListPackages {
            services: fixture.services(),
        }
        .execute(false)
        .unwrap_err();
        assert!(matches!(
            error,
            AppError::Config(ConfigError::NotConfigured)
        ));
    }

    #[test]
    fn compares_with_the_active_generation() {
        let mut firefox = package("firefox");
        firefox.metadata.store_name = Some("firefox-1".to_owned());
        let mut git = package("git");
        git.metadata.store_name = Some("git-2".to_owned());
        let fixture = Fixture::new(LIST)
            .with(firefox)
            .with(git)
            .with_store_names(&["firefox-1", "git-1", "vscode-9"]);
        let listed = list(&fixture, true);
        let state = |name: &str| {
            listed
                .packages
                .iter()
                .find(|package| package.name == name)
                .and_then(|package| package.active)
        };
        assert_eq!(state("firefox"), Some(ActiveState::Active));
        assert_eq!(state("git"), Some(ActiveState::NotActive));
        assert_eq!(state("vscode"), Some(ActiveState::Active));
        assert_eq!(state("tool"), Some(ActiveState::Unknown));
    }
}
