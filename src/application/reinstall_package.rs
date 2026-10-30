//! Use case: reapply the configuration for a declared package.
//!
//! The Nix store is immutable and the system is declarative, so there is nothing to
//! remove and add back. Reinstalling validates the configuration and rebuilds it,
//! which does not necessarily download or compile anything again. The file is never
//! changed.

use super::error::AppError;
use super::managed::{self, Managed};
use super::ports::{Notice, Services};
use super::remove_package::single_declaration;
use crate::domain::package::{PackageName, PackageSource};
use crate::domain::resolution::PackageRequest;

/// Validates a declaration and rebuilds the system.
pub struct ReinstallPackage<'a> {
    pub services: Services<'a>,
}

impl ReinstallPackage<'_> {
    /// Runs the use case for the name typed by the user.
    pub fn execute(&self, name: &str) -> Result<(), AppError> {
        let services = self.services;
        let name = PackageName::parse(name)?;
        let Managed { file, declared } = managed::open_for_reading(services)?;
        let request = PackageRequest::resolve(&name, &declared.aliases);

        let hint = format!(". To add it, run 'nixblend install {name}'");
        let declaration = single_declaration(
            &declared.declarations,
            &request,
            name.as_str(),
            file.path(),
            &hint,
        )?;
        managed::check_current(services, file.as_ref())?;

        let source = declaration
            .identity
            .as_ref()
            .map_or(&PackageSource::Unknown, |identity| &identity.source);
        services.ui.notify(&Notice::Reinstalling {
            declaration: declaration.text.clone(),
            source: source.label().to_owned(),
        })?;
        if let Err(error) = services.rebuilder.rebuild() {
            services.ui.notify(&Notice::RebuildFailed)?;
            return Err(error.into());
        }
        services.ui.notify(&Notice::RebuildDone)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::error::EditError;
    use crate::application::test_support::Fixture;

    const LIST: &str = "{ pkgs, ... }: with pkgs; [ git ]";

    fn run(fixture: &Fixture, name: &str) -> Result<(), AppError> {
        ReinstallPackage {
            services: fixture.services(),
        }
        .execute(name)
    }

    #[test]
    fn rebuilds_without_touching_the_file() {
        let fixture = Fixture::new(LIST);
        run(&fixture, "git").unwrap();
        assert_eq!(fixture.rebuilds(), 1);
        assert_eq!(fixture.content(), LIST);
        assert!(!fixture.backed_up());
        assert!(fixture.notices().iter().any(|notice| matches!(
            notice,
            Notice::Reinstalling { declaration, source } if declaration == "git" && source == "nixpkgs"
        )));
    }

    #[test]
    fn undeclared_package_suggests_install() {
        let fixture = Fixture::new(LIST);
        let error = run(&fixture, "vim").unwrap_err();
        assert!(
            error.to_string().contains("nixblend install vim"),
            "{error}"
        );
        assert_eq!(fixture.rebuilds(), 0);
    }

    #[test]
    fn invalid_configuration_is_not_rebuilt() {
        let fixture = Fixture::new(LIST).with_invalid_current();
        let error = run(&fixture, "git").unwrap_err();
        assert!(matches!(
            error,
            AppError::Edit(EditError::InvalidCurrentNix(_))
        ));
        assert_eq!(fixture.rebuilds(), 0);
    }

    #[test]
    fn failed_rebuild_is_reported() {
        let fixture = Fixture::new(LIST).with_failing_rebuild();
        assert!(matches!(run(&fixture, "git"), Err(AppError::Rebuild(_))));
        assert_eq!(fixture.content(), LIST);
    }
}
