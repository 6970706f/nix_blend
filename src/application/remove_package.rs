//! Use case: remove a package's declaration from the managed file.

use super::error::AppError;
use super::managed::{self, Managed};
use super::ports::{Notice, Question, Services, SyntaxCheck};
use super::rebuild_step;
use crate::domain::package::{PackageDeclaration, PackageName};
use crate::domain::resolution::{DeclarationMatch, PackageRequest, find_declaration};

/// Removes exactly one declaration, leaving the rest of the file as it is.
pub struct RemovePackage<'a> {
    pub services: Services<'a>,
}

impl RemovePackage<'_> {
    /// Runs the use case for the name typed by the user.
    pub fn execute(&self, name: &str, no_rebuild: bool) -> Result<(), AppError> {
        let services = self.services;
        let name = PackageName::parse(name)?;
        let Managed { mut file, declared } = managed::open_for_edit(services)?;
        let request = PackageRequest::resolve(&name, &declared.aliases);

        let declaration = single_declaration(
            &declared.declarations,
            &request,
            name.as_str(),
            file.path(),
            "",
        )?;
        managed::check_current(services, file.as_ref())?;
        services.ui.notify(&Notice::WillRemove {
            declaration: declaration.text.clone(),
            line: declaration.line,
        })?;

        let new_content = services.document.remove(file.content(), declaration)?;
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
        services.ui.notify(&Notice::PackageRemoved {
            package: declaration.text.clone(),
            path: file.path().to_owned(),
        })?;
        rebuild_step::run_after_edit(services, file.as_mut(), &original, no_rebuild)
    }
}

/// Turns a match into the one declaration it must be, or an explanation of why
/// there is not exactly one.
pub(super) fn single_declaration<'a>(
    declarations: &'a [PackageDeclaration],
    request: &PackageRequest,
    typed: &str,
    path: &std::path::Path,
    extra_hint: &str,
) -> Result<&'a PackageDeclaration, AppError> {
    match find_declaration(declarations, request) {
        DeclarationMatch::One(declaration) => Ok(declaration),
        DeclarationMatch::None { elsewhere } => {
            let mut hint = String::new();
            if !elsewhere.is_empty() {
                hint.push_str(". Declared from another source as: ");
                hint.push_str(&describe(&elsewhere));
            }
            hint.push_str(extra_hint);
            Err(AppError::NotDeclared {
                package: typed.to_owned(),
                path: path.to_owned(),
                hint,
            })
        }
        DeclarationMatch::Ambiguous(matches) => Err(AppError::AmbiguousDeclaration {
            package: typed.to_owned(),
            path: path.to_owned(),
            matches: describe(&matches),
        }),
    }
}

fn describe(declarations: &[&PackageDeclaration]) -> String {
    declarations
        .iter()
        .map(|declaration| format!("'{}' (line {})", declaration.text, declaration.line))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::test_support::Fixture;

    const BOTH: &str = "{ pkgs, inputs, ... }:\nlet unstable = import inputs.nixpkgs-unstable { };\nin with pkgs; [\n  # browsers\n  firefox\n  unstable.firefox\n  git # vcs\n]\n";

    fn run(fixture: &Fixture, name: &str) -> Result<(), AppError> {
        RemovePackage {
            services: fixture.services(),
        }
        .execute(name, false)
    }

    #[test]
    fn removes_after_confirmation_and_offers_the_rebuild() {
        let fixture = Fixture::new(BOTH).answering(&[true, true]);
        run(&fixture, "git").unwrap();
        assert!(!fixture.content().contains("git"));
        assert!(fixture.content().contains("# browsers"));
        assert!(fixture.backed_up());
        assert_eq!(fixture.rebuilds(), 1);
        assert!(fixture.notices().iter().any(|notice| matches!(
            notice,
            Notice::WillRemove { declaration, line: 7 } if declaration == "git"
        )));
    }

    #[test]
    fn no_rebuild_option_skips_the_rebuild() {
        let fixture = Fixture::new(BOTH).answering(&[true]);
        RemovePackage {
            services: fixture.services(),
        }
        .execute("git", true)
        .unwrap();
        assert_eq!(fixture.rebuilds(), 0);
        assert!(!fixture.content().contains("git"));
    }

    #[test]
    fn ambiguous_name_changes_nothing() {
        let fixture = Fixture::new(BOTH);
        let error = run(&fixture, "firefox").unwrap_err();
        match error {
            AppError::AmbiguousDeclaration { matches, .. } => {
                assert!(matches.contains("'firefox' (line 5)"), "{matches}");
                assert!(matches.contains("'unstable.firefox' (line 6)"), "{matches}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fixture.content(), BOTH);
    }

    #[test]
    fn a_qualified_name_selects_one_declaration() {
        let fixture = Fixture::new(BOTH).answering(&[true, false]);
        run(&fixture, "unstable.firefox").unwrap();
        assert!(!fixture.content().contains("unstable.firefox"));
        assert!(fixture.content().contains("  firefox\n"));
    }

    #[test]
    fn explicit_nixpkgs_selects_one_declaration() {
        let fixture = Fixture::new(BOTH).answering(&[true, false]);
        run(&fixture, "pkgs.firefox").unwrap();
        assert!(fixture.content().contains("unstable.firefox"));
        assert!(!fixture.content().contains("  firefox\n"));
    }

    #[test]
    fn missing_declaration_changes_nothing() {
        let fixture = Fixture::new(BOTH);
        assert!(matches!(
            run(&fixture, "vim"),
            Err(AppError::NotDeclared { .. })
        ));
        assert_eq!(fixture.content(), BOTH);
    }

    #[test]
    fn refusing_the_diff_changes_nothing() {
        let fixture = Fixture::new(BOTH).answering(&[false]);
        run(&fixture, "git").unwrap();
        assert_eq!(fixture.content(), BOTH);
        assert!(!fixture.backed_up());
    }

    #[test]
    fn invalid_result_is_refused_before_asking() {
        let fixture = Fixture::new(BOTH).with_invalid_candidate();
        assert!(run(&fixture, "git").is_err());
        assert_eq!(fixture.content(), BOTH);
        assert_eq!(fixture.confirmations(), 0);
    }
}
