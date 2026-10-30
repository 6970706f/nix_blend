//! Turns the results of use cases into text.

use std::fmt::Write as _;
use std::path::Path;

use crate::application::get_package_info::{DeclaredState, PackageInfo};
use crate::application::list_packages::{ActiveState, PackageList};
use crate::application::ports::{Notice, Question};
use crate::domain::package::{Blocker, Package};

const UNKNOWN: &str = "Unknown";
const CONTEXT_LINES: usize = 3;

/// Text of a notice.
pub fn notice(notice: &Notice) -> String {
    match notice {
        Notice::PathNotSaved => "The path was not saved.".to_owned(),
        Notice::ManagedFileConfiguredIn(path) => format!(
            "The managed file is set in {}. Edit or delete it to choose another file.",
            path.display()
        ),
        Notice::RevisionNotPinned => {
            "Warning: could not read the nixpkgs revision from the system flake.lock; using the flake registry."
                .to_owned()
        }
        Notice::Checking(package) => format!("Checking '{package}'..."),
        Notice::PackageFound(package) => format!("Found: {}", one_line(package)),
        Notice::DidYouMean(names) => format!(
            "Did you mean: {}? To install or inspect one of them, use its name.",
            names.join(", ")
        ),
        Notice::NoCloseMatch(package) => {
            format!("No attribute named or close to '{package}'.")
        }
        Notice::AlreadyDeclared {
            requested,
            declaration,
            path,
        } => {
            let written_as = if declaration == requested {
                String::new()
            } else {
                format!(" as '{declaration}'")
            };
            format!(
                "Package '{requested}' is already in {}{written_as}. Nothing was changed.",
                path.display()
            )
        }
        Notice::DeclaredElsewhere {
            requested,
            declarations,
        } => format!(
            "The same package name is already declared from another source: {}. Adding '{requested}' would declare it twice, and both may collide in the rebuild.",
            declarations.join(", ")
        ),
        Notice::SyntaxNotChecked => {
            "Warning: nix-instantiate not found; the file's syntax was not checked.".to_owned()
        }
        Notice::Diff { path, old, new } => render_diff(path, old, new),
        Notice::EditCancelled => "Edit cancelled. Nothing was changed.".to_owned(),
        Notice::NothingChanged => "Nothing was changed.".to_owned(),
        Notice::PackageAdded { package, path } => {
            format!("Edit: package '{package}' added to {}.", path.display())
        }
        Notice::WillRemove { declaration, line } => {
            format!("Declaration to remove: '{declaration}' (line {line})")
        }
        Notice::PackageRemoved { package, path } => {
            format!("Edit: '{package}' removed from {}.", path.display())
        }
        Notice::Reinstalling {
            declaration,
            source,
        } => format!(
            "Reapplying the configuration for '{declaration}' (source: {source}). This runs a rebuild and does not necessarily download or compile the package again."
        ),
        Notice::RebuildSkipped => "Rebuild: not run. The edit was kept.".to_owned(),
        Notice::RebuildDone => "Rebuild: done.".to_owned(),
        Notice::RebuildFailed => "Rebuild: failed.".to_owned(),
        Notice::EditUndone => "Edit: the file is back to its previous content.".to_owned(),
        Notice::UndoFailed(reason) => {
            format!("Edit: could not undo, so the edit is still applied: {reason}")
        }
        Notice::Warning(warning) => format!("Warning: {warning}"),
    }
}

/// Text of a question.
pub fn question(question: &Question) -> String {
    match question {
        Question::ApplyEdit => "Apply this change?".to_owned(),
        Question::RunRebuild => "Run the NixOS rebuild now?".to_owned(),
        Question::AddUnavailable(package) => format!(
            "The system configuration refuses '{}' ({}), so a rebuild would fail. Add it anyway?",
            package.identity,
            blockers(package)
        ),
        Question::AddAlongside { package, source } => {
            format!("Declare '{package}' from {source} as well?")
        }
        Question::RunFullSearch(package) => format!(
            "Search all package names and descriptions for '{package}'? The first search of a nixpkgs revision takes about a minute and several GB of memory."
        ),
        Question::SearchDespiteTypo => "Search for it anyway?".to_owned(),
        Question::UndoEdit => "Undo the edit?".to_owned(),
    }
}

fn blockers(package: &Package) -> String {
    package
        .blockers()
        .iter()
        .map(|blocker| match blocker {
            Blocker::Broken => "marked as broken",
            Blocker::Unfree => "unfree, and not allowed by the configuration",
            Blocker::Insecure => "marked as insecure",
            Blocker::Platform => "not supported on this platform",
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One line with the attribute, version and description.
pub fn one_line(package: &Package) -> String {
    clean(&format!(
        "{} ({}) - {}",
        package.identity,
        package.metadata.version.as_deref().unwrap_or(UNKNOWN),
        package.metadata.description.as_deref().unwrap_or(UNKNOWN)
    ))
}

/// Text from files and tools comes from outside the program. Control characters could
/// rewrite what the terminal shows, so only line breaks and tabs get through.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// Like [`sanitize`], for what must stay on one line.
fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

fn field(output: &mut String, label: &str, value: Option<&str>) {
    let _ = writeln!(output, "{label}: {}", clean(value.unwrap_or(UNKNOWN)));
}

fn flag(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "Yes",
        Some(false) => "No",
        None => UNKNOWN,
    }
}

fn availability(package: &Package) -> String {
    match package.metadata.available {
        Some(true) => "Available".to_owned(),
        Some(false) => format!("Refused: {}", blockers(package)),
        None => UNKNOWN.to_owned(),
    }
}

fn common_fields(output: &mut String, package: &Package) {
    field(output, "Package", package.metadata.pname.as_deref());
    field(output, "Attribute", Some(&package.identity.qualified()));
    field(output, "Version", package.metadata.version.as_deref());
    field(
        output,
        "Description",
        package.metadata.description.as_deref(),
    );
    field(output, "License", package.metadata.license.as_deref());
    field(output, "Source", Some(package.identity.source.label()));
}

/// One search result. A search does not report licenses or availability by itself, so
/// those may be unknown.
pub fn search_result(package: &Package) -> String {
    let mut output = String::new();
    common_fields(&mut output, package);
    field(&mut output, "Status", Some(&availability(package)));
    output
}

/// Everything known about a package.
pub fn package_info(info: &PackageInfo) -> String {
    let package = &info.package;
    let mut output = String::new();
    common_fields(&mut output, package);
    field(
        &mut output,
        "Homepage",
        package.metadata.homepage.as_deref(),
    );
    field(&mut output, "Status", Some(&availability(package)));
    field(&mut output, "Unfree", Some(flag(package.metadata.unfree)));
    field(&mut output, "Broken", Some(flag(package.metadata.broken)));
    field(
        &mut output,
        "Insecure",
        Some(flag(package.metadata.insecure)),
    );
    let declared = match &info.declared {
        DeclaredState::Yes(declarations) => format!("Yes ({})", declarations.join(", ")),
        DeclaredState::No => "No".to_owned(),
        DeclaredState::Unknown(reason) => format!("{UNKNOWN} ({reason})"),
    };
    field(&mut output, "Declared", Some(&declared));
    if let Some(warning) = &info.warning {
        field(&mut output, "Warning", Some(warning));
    }
    output
}

/// The declarations as a table.
pub fn package_list(list: &PackageList) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "Declared in {}", list.path.display());
    if list.packages.is_empty() {
        output.push_str("No packages are declared.\n");
        return output;
    }

    let name_width = list
        .packages
        .iter()
        .map(|package| package.name.len())
        .max()
        .unwrap_or(0)
        .max("PACKAGE".len());
    let source_width = list
        .packages
        .iter()
        .map(|package| package.source.len())
        .max()
        .unwrap_or(0)
        .max("SOURCE".len());
    let compared = list.packages.iter().any(|package| package.active.is_some());

    if compared {
        let _ = writeln!(
            output,
            "{:name_width$}  {:source_width$}  ACTIVE",
            "PACKAGE", "SOURCE"
        );
    } else {
        let _ = writeln!(output, "{:name_width$}  SOURCE", "PACKAGE");
    }
    for package in &list.packages {
        match package.active {
            Some(state) => {
                let active = match state {
                    ActiveState::Active => "yes",
                    ActiveState::NotActive => "no",
                    ActiveState::Unknown => "unknown",
                };
                let _ = writeln!(
                    output,
                    "{:name_width$}  {:source_width$}  {active}",
                    package.name, package.source
                );
            }
            None => {
                let _ = writeln!(output, "{:name_width$}  {}", package.name, package.source);
            }
        }
    }
    if let Some(warning) = &list.warning {
        let _ = writeln!(output, "Warning: {warning}");
    }
    output
}

/// Generates a unified diff between two texts.
///
/// Nixblend changes a single region per operation, so the common prefix and suffix
/// delimit the exact change without a general diff algorithm.
pub fn render_diff(path: &Path, old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();

    let prefix = old_lines
        .iter()
        .zip(&new_lines)
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = old_lines[prefix..]
        .iter()
        .rev()
        .zip(new_lines[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();

    let old_end = old_lines.len() - suffix;
    let new_end = new_lines.len() - suffix;
    let context_start = prefix.saturating_sub(CONTEXT_LINES);
    let context_end = (suffix.min(CONTEXT_LINES)) + old_end;

    let mut diff = String::new();
    let _ = writeln!(diff, "--- {}", path.display());
    let _ = writeln!(diff, "+++ {}", path.display());
    let _ = writeln!(
        diff,
        "@@ -{} +{} @@",
        hunk_range(context_start, context_end - context_start),
        hunk_range(
            context_start,
            prefix - context_start + (new_end - prefix) + (context_end - old_end)
        ),
    );
    push_lines(&mut diff, ' ', &old_lines[context_start..prefix]);
    push_lines(&mut diff, '-', &old_lines[prefix..old_end]);
    push_lines(&mut diff, '+', &new_lines[prefix..new_end]);
    push_lines(&mut diff, ' ', &old_lines[old_end..context_end]);
    diff
}

/// Formats `start,length`. An empty range points to the previous line.
fn hunk_range(start: usize, length: usize) -> String {
    let first = if length == 0 { start } else { start + 1 };
    format!("{first},{length}")
}

fn push_lines(diff: &mut String, marker: char, lines: &[&str]) {
    for line in lines {
        diff.push(marker);
        diff.push_str(line.trim_end_matches(['\r', '\n']));
        diff.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::list_packages::ListedPackage;
    use crate::domain::package::{PackageIdentity, PackageMetadata};
    use std::path::PathBuf;

    fn listed(name: &str, source: &str, active: Option<ActiveState>) -> ListedPackage {
        ListedPackage {
            name: name.to_owned(),
            source: source.to_owned(),
            active,
        }
    }

    #[test]
    fn missing_metadata_is_shown_as_unknown_and_not_invented() {
        let info = PackageInfo {
            package: Package {
                identity: PackageIdentity::nixpkgs("x"),
                metadata: PackageMetadata::default(),
            },
            declared: DeclaredState::No,
            warning: None,
        };
        let text = package_info(&info);
        for line in [
            "Package: Unknown",
            "Attribute: x",
            "Version: Unknown",
            "Description: Unknown",
            "License: Unknown",
            "Source: nixpkgs",
            "Homepage: Unknown",
            "Status: Unknown",
            "Unfree: Unknown",
            "Broken: Unknown",
            "Declared: No",
        ] {
            assert!(text.contains(line), "{line} not in {text}");
        }
    }

    #[test]
    fn known_metadata_is_shown() {
        let info = PackageInfo {
            package: Package {
                identity: PackageIdentity::nixpkgs("firefox"),
                metadata: PackageMetadata {
                    pname: Some("firefox".to_owned()),
                    license: Some("MPL-2.0".to_owned()),
                    available: Some(false),
                    unfree: Some(true),
                    ..PackageMetadata::default()
                },
            },
            declared: DeclaredState::Yes(vec!["firefox".to_owned()]),
            warning: Some("partial".to_owned()),
        };
        let text = package_info(&info);
        assert!(text.contains("License: MPL-2.0"));
        assert!(text.contains("Status: Refused: unfree"));
        assert!(text.contains("Unfree: Yes"));
        assert!(text.contains("Declared: Yes (firefox)"));
        assert!(text.contains("Warning: partial"));
    }

    #[test]
    fn control_characters_do_not_reach_the_terminal() {
        let package = Package {
            identity: PackageIdentity::nixpkgs("x"),
            metadata: PackageMetadata {
                description: Some("bad\x1b[31m\ntext".to_owned()),
                ..PackageMetadata::default()
            },
        };
        assert!(!one_line(&package).contains('\x1b'));
        assert!(!search_result(&package).contains('\x1b'));
    }

    #[test]
    fn list_aligns_columns() {
        let list = PackageList {
            path: PathBuf::from("/p.nix"),
            packages: vec![
                listed("firefox", "nixpkgs", None),
                listed("vscode", "unstable", None),
            ],
            warning: None,
        };
        assert_eq!(
            package_list(&list),
            "Declared in /p.nix\nPACKAGE  SOURCE\nfirefox  nixpkgs\nvscode   unstable\n"
        );
    }

    #[test]
    fn list_shows_the_active_generation_when_compared() {
        let list = PackageList {
            path: PathBuf::from("/p.nix"),
            packages: vec![
                listed("git", "nixpkgs", Some(ActiveState::Active)),
                listed("vim", "nixpkgs", Some(ActiveState::NotActive)),
                listed("x", "tool", Some(ActiveState::Unknown)),
            ],
            warning: None,
        };
        let text = package_list(&list);
        assert!(text.contains("PACKAGE  SOURCE   ACTIVE"), "{text}");
        assert!(text.contains("git      nixpkgs  yes"), "{text}");
        assert!(text.contains("vim      nixpkgs  no"), "{text}");
        assert!(text.contains("x        tool     unknown"), "{text}");
    }

    #[test]
    fn empty_list_says_so() {
        let list = PackageList {
            path: PathBuf::from("/p.nix"),
            packages: Vec::new(),
            warning: None,
        };
        assert!(package_list(&list).contains("No packages are declared."));
    }

    #[test]
    fn diff_shows_insertion_with_context() {
        let old = "1\n2\n3\n4\n]\n5\n6\n7\n8\n";
        let new = "1\n2\n3\n4\nfoo\n]\n5\n6\n7\n8\n";
        assert_eq!(
            render_diff(Path::new("/p.nix"), old, new),
            "--- /p.nix\n+++ /p.nix\n@@ -2,6 +2,7 @@\n 2\n 3\n 4\n+foo\n ]\n 5\n 6\n"
        );
    }

    #[test]
    fn diff_shows_line_replacement() {
        assert_eq!(
            render_diff(Path::new("p"), "[ a ]\n", "[ a b ]\n"),
            "--- p\n+++ p\n@@ -1,1 +1,1 @@\n-[ a ]\n+[ a b ]\n"
        );
    }

    #[test]
    fn diff_shows_removal() {
        assert_eq!(
            render_diff(Path::new("p"), "a\nb\nc\n", "a\nc\n"),
            "--- p\n+++ p\n@@ -1,3 +1,2 @@\n a\n-b\n c\n"
        );
    }

    #[test]
    fn diff_handles_change_at_file_edges_without_final_newline() {
        assert_eq!(
            render_diff(Path::new("p"), "[ ]", "[ a ]"),
            "--- p\n+++ p\n@@ -1,1 +1,1 @@\n-[ ]\n+[ a ]\n"
        );
        assert_eq!(
            render_diff(Path::new("p"), "", "a\n"),
            "--- p\n+++ p\n@@ -0,0 +1,1 @@\n+a\n"
        );
    }

    #[test]
    fn diff_does_not_print_carriage_returns() {
        let diff = render_diff(Path::new("p"), "a\r\n", "a\r\nb\r\n");
        assert!(diff.ends_with(" a\n+b\n"), "{diff}");
    }

    #[test]
    fn diff_handles_repeated_lines() {
        let diff = render_diff(Path::new("p"), "a\na\n", "a\na\na\n");
        assert!(diff.ends_with("@@ -1,2 +1,3 @@\n a\n a\n+a\n"), "{diff}");
    }
}
