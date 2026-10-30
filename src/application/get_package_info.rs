//! Use case: show what is known about a package.

use super::error::AppError;
use super::managed;
use super::ports::Services;
use crate::domain::package::{ChannelAlias, Package, PackageName};
use crate::domain::resolution::{DeclarationMatch, PackageRequest, find_declaration};

/// Whether the package is declared in the managed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredState {
    Yes(Vec<String>),
    No,
    /// There is no managed file to look at, or it could not be read.
    Unknown,
}

/// Result of the query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInfo {
    pub package: Package,
    pub declared: DeclaredState,
}

/// Queries the metadata of a package.
pub struct GetPackageInfo<'a> {
    pub services: Services<'a>,
}

impl GetPackageInfo<'_> {
    /// Runs the use case for the name typed by the user.
    pub fn execute(&self, name: &str) -> Result<PackageInfo, AppError> {
        let services = self.services;
        let name = PackageName::parse(name)?;
        let managed = managed::open_for_reading(services).ok();
        let aliases: &[ChannelAlias] = managed
            .as_ref()
            .map_or(&[], |managed| &managed.declared.aliases);
        let request = PackageRequest::resolve(&name, aliases);

        let package = services
            .catalogs
            .catalog(&request.identity.source)?
            .exact(&request.identity.attr)?
            .ok_or_else(|| AppError::PackageNotFound(request.identity.qualified()))?;

        let declared = match &managed {
            None => DeclaredState::Unknown,
            Some(managed) => match find_declaration(&managed.declared.declarations, &request) {
                DeclarationMatch::None { .. } => DeclaredState::No,
                DeclarationMatch::One(declaration) => {
                    DeclaredState::Yes(vec![declaration.text.clone()])
                }
                DeclarationMatch::Ambiguous(matches) => DeclaredState::Yes(
                    matches
                        .iter()
                        .map(|declaration| declaration.text.clone())
                        .collect(),
                ),
            },
        };
        Ok(PackageInfo { package, declared })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::error::SearchError;
    use crate::application::test_support::Fixture;

    const LIST: &str = "{ pkgs, ... }: with pkgs; [ git ]";

    fn info(fixture: &Fixture, name: &str) -> Result<PackageInfo, AppError> {
        GetPackageInfo {
            services: fixture.services(),
        }
        .execute(name)
    }

    #[test]
    fn tells_whether_the_package_is_declared() {
        let fixture = Fixture::new(LIST).with_package("git").with_package("vim");
        assert_eq!(
            info(&fixture, "git").unwrap().declared,
            DeclaredState::Yes(vec!["git".to_owned()])
        );
        assert_eq!(info(&fixture, "vim").unwrap().declared, DeclaredState::No);
    }

    #[test]
    fn works_without_a_configuration() {
        let fixture = Fixture::new(LIST).with_package("git").without_config();
        assert_eq!(
            info(&fixture, "git").unwrap().declared,
            DeclaredState::Unknown
        );
    }

    #[test]
    fn unknown_package_is_reported() {
        let fixture = Fixture::new(LIST);
        assert!(matches!(
            info(&fixture, "nope"),
            Err(AppError::PackageNotFound(_))
        ));
    }

    #[test]
    fn failed_query_is_not_a_missing_package() {
        let fixture = Fixture::new(LIST).failing_catalog();
        assert!(matches!(
            info(&fixture, "git"),
            Err(AppError::Search(SearchError::Failed(_)))
        ));
    }
}
