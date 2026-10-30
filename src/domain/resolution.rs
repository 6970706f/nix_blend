//! Rules that relate what the user asked for to packages and declarations.

use super::package::{
    ChannelAlias, DeclarationKind, Package, PackageDeclaration, PackageIdentity, PackageName,
    PackageSource,
};

const PKGS_PREFIX: &str = "pkgs.";
/// Largest edit distance still considered a typo.
const MAX_TYPO_DISTANCE: usize = 2;

/// A package name interpreted against the package sets a file imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRequest {
    pub identity: PackageIdentity,
    /// The user named the source, as in `pkgs.git` or `unstable.git`.
    pub explicit_source: bool,
}

impl PackageRequest {
    /// Interprets `name`. A first segment that is not a known package set is part
    /// of the attribute, as in `kdePackages.kate`.
    pub fn resolve(name: &PackageName, aliases: &[ChannelAlias]) -> Self {
        let text = name.as_str();
        if let Some(attr) = text.strip_prefix(PKGS_PREFIX) {
            return Self {
                identity: PackageIdentity::nixpkgs(attr),
                explicit_source: true,
            };
        }
        let channel = text.split_once('.').and_then(|(first, attr)| {
            let alias = aliases.iter().find(|alias| alias.name == first)?;
            Some(PackageIdentity {
                source: PackageSource::Channel {
                    alias: alias.name.clone(),
                    input: alias.input.clone(),
                },
                attr: attr.to_owned(),
            })
        });
        match channel {
            Some(identity) => Self {
                identity,
                explicit_source: true,
            },
            None => Self {
                identity: PackageIdentity::nixpkgs(text),
                explicit_source: false,
            },
        }
    }
}

/// How a package to install relates to the existing declarations.
#[derive(Debug, PartialEq, Eq)]
pub enum InstallCheck<'a> {
    NotDeclared,
    /// The same package from the same source is already declared.
    AlreadyDeclared(&'a PackageDeclaration),
    /// The same name is declared from other sources only.
    DeclaredElsewhere(Vec<&'a PackageDeclaration>),
}

/// Compares a package to install with the declarations. A name that cannot be
/// traced to a source counts as the same package, since adding it could collide.
pub fn check_install<'a>(
    declarations: &'a [PackageDeclaration],
    identity: &PackageIdentity,
) -> InstallCheck<'a> {
    let same_name: Vec<&PackageDeclaration> = declarations
        .iter()
        .filter(|declaration| {
            matches!(
                declaration.kind,
                DeclarationKind::Plain | DeclarationKind::Override
            ) && declaration
                .identity
                .as_ref()
                .is_some_and(|declared| declared.attr == identity.attr)
        })
        .collect();

    let same_source = same_name.iter().find(|declaration| {
        declaration.identity.as_ref().is_some_and(|declared| {
            declared.source == identity.source || declared.source == PackageSource::Unknown
        })
    });
    match (same_source, same_name.is_empty()) {
        (Some(declaration), _) => InstallCheck::AlreadyDeclared(declaration),
        (None, true) => InstallCheck::NotDeclared,
        (None, false) => InstallCheck::DeclaredElsewhere(same_name),
    }
}

/// Declarations a removal or reinstallation request refers to.
#[derive(Debug, PartialEq, Eq)]
pub enum DeclarationMatch<'a> {
    /// Nothing matches. `elsewhere` lists the same name from other sources.
    None {
        elsewhere: Vec<&'a PackageDeclaration>,
    },
    One(&'a PackageDeclaration),
    Ambiguous(Vec<&'a PackageDeclaration>),
}

/// Finds what `request` refers to. Without an explicit source, any source matches,
/// and more than one match must be resolved by the user.
pub fn find_declaration<'a>(
    declarations: &'a [PackageDeclaration],
    request: &PackageRequest,
) -> DeclarationMatch<'a> {
    let same_name = declarations.iter().filter(|declaration| {
        declaration
            .identity
            .as_ref()
            .is_some_and(|declared| declared.attr == request.identity.attr)
    });
    let (matching, elsewhere): (Vec<_>, Vec<_>) = same_name.partition(|declaration| {
        !request.explicit_source
            || declaration
                .identity
                .as_ref()
                .is_some_and(|declared| declared.source == request.identity.source)
    });
    match matching.as_slice() {
        [] => DeclarationMatch::None { elsewhere },
        [declaration] => DeclarationMatch::One(declaration),
        _ => DeclarationMatch::Ambiguous(matching),
    }
}

/// Removes repeated identities, keeping the order of the file.
pub fn unique_declarations(declarations: &[PackageDeclaration]) -> Vec<&PackageDeclaration> {
    let mut seen = Vec::new();
    declarations
        .iter()
        .filter(|declaration| {
            let key = declaration
                .identity
                .as_ref()
                .map_or_else(|| declaration.text.clone(), PackageIdentity::qualified);
            if seen.contains(&key) {
                false
            } else {
                seen.push(key);
                true
            }
        })
        .collect()
}

/// Outcome of ranking search results for an installation.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    Found(Package),
    /// Similar packages for the user to choose from, best match first.
    Candidates(Vec<Package>),
    NotFound,
}

/// How well a package matches a term, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchRank {
    ExactAttr,
    ExactName,
    Similar,
    Description,
}

/// Ranks `package` against `term`, or `None` if it does not match at all.
pub fn match_rank(package: &Package, term: &str) -> Option<MatchRank> {
    let attr = &package.identity.attr;
    if attr == term {
        return Some(MatchRank::ExactAttr);
    }
    let pname = package.metadata.pname.as_deref().unwrap_or("");
    let last_segment = attr.rsplit('.').next().unwrap_or(attr);
    if last_segment.eq_ignore_ascii_case(term) || pname.eq_ignore_ascii_case(term) {
        return Some(MatchRank::ExactName);
    }
    let term = term.to_lowercase();
    if attr.to_lowercase().contains(&term) || pname.to_lowercase().contains(&term) {
        return Some(MatchRank::Similar);
    }
    package
        .metadata
        .description
        .as_deref()
        .is_some_and(|description| description.to_lowercase().contains(&term))
        .then_some(MatchRank::Description)
}

/// Orders all results for display, best match first.
pub fn rank_results(results: Vec<Package>, term: &str) -> Vec<Package> {
    let mut ranked: Vec<(Option<MatchRank>, Package)> = results
        .into_iter()
        .map(|package| (match_rank(&package, term), package))
        .collect();
    ranked.sort_by(|(rank_a, a), (rank_b, b)| {
        let key = |rank: &Option<MatchRank>| rank.unwrap_or(MatchRank::Description);
        (key(rank_a), a.identity.attr.len(), &a.identity.attr).cmp(&(
            key(rank_b),
            b.identity.attr.len(),
            &b.identity.attr,
        ))
    });
    ranked.into_iter().map(|(_, package)| package).collect()
}

/// Decides what a search means for an installation. Results that only mention the
/// name in their description do not confirm that the package exists. A single
/// exact name is accepted because the attribute is still shown in the diff; a
/// merely similar one always needs a choice.
pub fn resolve_results(results: Vec<Package>, name: &str) -> Resolution {
    let mut matches: Vec<(MatchRank, Package)> = results
        .into_iter()
        .filter_map(|package| {
            let rank = match_rank(&package, name)?;
            (rank != MatchRank::Description).then_some((rank, package))
        })
        .collect();
    matches.sort_by(|(rank_a, a), (rank_b, b)| {
        (rank_a, a.identity.attr.len(), &a.identity.attr).cmp(&(
            rank_b,
            b.identity.attr.len(),
            &b.identity.attr,
        ))
    });

    let exact_names = matches
        .iter()
        .filter(|(rank, _)| *rank == MatchRank::ExactName)
        .count();
    let best = matches.first().map(|(rank, _)| *rank);
    if best == Some(MatchRank::ExactAttr)
        || (best == Some(MatchRank::ExactName) && exact_names == 1)
    {
        return Resolution::Found(matches.swap_remove(0).1);
    }
    if matches.is_empty() {
        return Resolution::NotFound;
    }
    Resolution::Candidates(matches.into_iter().map(|(_, package)| package).collect())
}

/// Attribute names close to `term`: likely typos and names containing it, best
/// first. This lets a typo be answered without a full search.
pub fn near_names(names: &[String], term: &str, limit: usize) -> Vec<String> {
    let term = term.to_lowercase();
    let mut near: Vec<(usize, &String)> = names
        .iter()
        .filter_map(|name| {
            let lower = name.to_lowercase();
            let distance = edit_distance(&lower, &term);
            if distance <= MAX_TYPO_DISTANCE {
                Some((distance, name))
            } else if term.len() >= 3 && lower.contains(&term) {
                Some((MAX_TYPO_DISTANCE + name.len() - term.len(), name))
            } else {
                None
            }
        })
        .collect();
    near.sort();
    near.into_iter()
        .take(limit)
        .map(|(_, name)| name.clone())
        .collect()
}

/// Attribute names a few edits away from `term`, closest first, unless `term` is itself
/// an attribute. These suggest a typo, as opposed to a partial name or a description.
pub fn likely_typos(names: &[String], term: &str, limit: usize) -> Vec<String> {
    if names.iter().any(|name| name == term) {
        return Vec::new();
    }
    let term = term.to_lowercase();
    let mut close: Vec<(usize, &String)> = names
        .iter()
        .map(|name| (edit_distance(&name.to_lowercase(), &term), name))
        .filter(|(distance, _)| *distance <= MAX_TYPO_DISTANCE)
        .collect();
    close.sort();
    close
        .into_iter()
        .take(limit)
        .map(|(_, name)| name.clone())
        .collect()
}

/// Levenshtein distance, giving up once it clearly exceeds the typo limit.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len().abs_diff(b.len()) > MAX_TYPO_DISTANCE {
        return MAX_TYPO_DISTANCE + 1;
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, byte_a) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, byte_b) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(byte_a != byte_b);
            current.push(substitution.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::package::PackageMetadata;

    fn name(text: &str) -> PackageName {
        PackageName::parse(text).unwrap()
    }

    fn unstable() -> ChannelAlias {
        ChannelAlias {
            name: "unstable".to_owned(),
            input: "nixpkgs-unstable".to_owned(),
        }
    }

    fn channel(attr: &str) -> PackageIdentity {
        PackageIdentity {
            source: PackageSource::Channel {
                alias: "unstable".to_owned(),
                input: "nixpkgs-unstable".to_owned(),
            },
            attr: attr.to_owned(),
        }
    }

    fn declared(identity: PackageIdentity, kind: DeclarationKind) -> PackageDeclaration {
        PackageDeclaration {
            text: identity.qualified(),
            identity: Some(identity),
            kind,
            line: 1,
            start: 0,
            end: 0,
        }
    }

    fn plain(identity: PackageIdentity) -> PackageDeclaration {
        declared(identity, DeclarationKind::Plain)
    }

    fn package(attr: &str, pname: &str, description: &str) -> Package {
        Package {
            identity: PackageIdentity::nixpkgs(attr),
            metadata: PackageMetadata {
                pname: Some(pname.to_owned()),
                version: Some("1.0".to_owned()),
                description: Some(description.to_owned()),
                ..PackageMetadata::default()
            },
        }
    }

    fn attrs(resolution: Resolution) -> Vec<String> {
        match resolution {
            Resolution::Candidates(packages) => packages
                .into_iter()
                .map(|package| package.identity.attr)
                .collect(),
            other => panic!("expected candidates, got {other:?}"),
        }
    }

    #[test]
    fn requests_recognize_sources() {
        let aliases = [unstable()];
        let bare = PackageRequest::resolve(&name("git"), &aliases);
        assert_eq!(bare.identity, PackageIdentity::nixpkgs("git"));
        assert!(!bare.explicit_source);

        let explicit = PackageRequest::resolve(&name("pkgs.git"), &aliases);
        assert_eq!(explicit.identity, PackageIdentity::nixpkgs("git"));
        assert!(explicit.explicit_source);

        let qualified = PackageRequest::resolve(&name("unstable.git"), &aliases);
        assert_eq!(qualified.identity, channel("git"));
        assert!(qualified.explicit_source);

        let nested = PackageRequest::resolve(&name("kdePackages.kate"), &aliases);
        assert_eq!(
            nested.identity,
            PackageIdentity::nixpkgs("kdePackages.kate")
        );
        assert!(!nested.explicit_source);
    }

    #[test]
    fn install_detects_the_same_package() {
        let declarations = [plain(PackageIdentity::nixpkgs("git"))];
        assert_eq!(
            check_install(&declarations, &PackageIdentity::nixpkgs("git")),
            InstallCheck::AlreadyDeclared(&declarations[0])
        );
        assert_eq!(
            check_install(&declarations, &PackageIdentity::nixpkgs("vim")),
            InstallCheck::NotDeclared
        );
    }

    #[test]
    fn install_reports_the_same_name_from_another_source() {
        let declarations = [plain(channel("vscode"))];
        assert_eq!(
            check_install(&declarations, &PackageIdentity::nixpkgs("vscode")),
            InstallCheck::DeclaredElsewhere(vec![&declarations[0]])
        );
        let declarations = [plain(PackageIdentity::nixpkgs("vscode"))];
        assert_eq!(
            check_install(&declarations, &channel("vscode")),
            InstallCheck::DeclaredElsewhere(vec![&declarations[0]])
        );
    }

    #[test]
    fn install_counts_overrides_but_not_expressions() {
        let declarations = [declared(
            PackageIdentity::nixpkgs("git"),
            DeclarationKind::Override,
        )];
        assert!(matches!(
            check_install(&declarations, &PackageIdentity::nixpkgs("git")),
            InstallCheck::AlreadyDeclared(_)
        ));
        let declarations = [declared(
            PackageIdentity::nixpkgs("git"),
            DeclarationKind::Expression,
        )];
        assert_eq!(
            check_install(&declarations, &PackageIdentity::nixpkgs("git")),
            InstallCheck::NotDeclared
        );
    }

    #[test]
    fn install_treats_untraceable_names_as_the_same_package() {
        let declarations = [plain(PackageIdentity {
            source: PackageSource::Unknown,
            attr: "git".to_owned(),
        })];
        assert!(matches!(
            check_install(&declarations, &PackageIdentity::nixpkgs("git")),
            InstallCheck::AlreadyDeclared(_)
        ));
    }

    #[test]
    fn removal_finds_a_single_declaration_from_any_source() {
        let declarations = [plain(channel("vscode"))];
        let request = PackageRequest::resolve(&name("vscode"), &[unstable()]);
        assert_eq!(
            find_declaration(&declarations, &request),
            DeclarationMatch::One(&declarations[0])
        );
    }

    #[test]
    fn removal_stops_on_ambiguous_names() {
        let declarations = [
            plain(PackageIdentity::nixpkgs("firefox")),
            plain(channel("firefox")),
        ];
        let request = PackageRequest::resolve(&name("firefox"), &[unstable()]);
        assert!(matches!(
            find_declaration(&declarations, &request),
            DeclarationMatch::Ambiguous(found) if found.len() == 2
        ));
    }

    #[test]
    fn a_named_source_resolves_the_ambiguity() {
        let declarations = [
            plain(PackageIdentity::nixpkgs("firefox")),
            plain(channel("firefox")),
        ];
        let aliases = [unstable()];
        assert_eq!(
            find_declaration(
                &declarations,
                &PackageRequest::resolve(&name("unstable.firefox"), &aliases)
            ),
            DeclarationMatch::One(&declarations[1])
        );
        assert_eq!(
            find_declaration(
                &declarations,
                &PackageRequest::resolve(&name("pkgs.firefox"), &aliases)
            ),
            DeclarationMatch::One(&declarations[0])
        );
    }

    #[test]
    fn repeated_identical_entries_are_ambiguous() {
        let declarations = [
            plain(PackageIdentity::nixpkgs("git")),
            plain(PackageIdentity::nixpkgs("git")),
        ];
        let request = PackageRequest::resolve(&name("git"), &[]);
        assert!(matches!(
            find_declaration(&declarations, &request),
            DeclarationMatch::Ambiguous(_)
        ));
    }

    #[test]
    fn removal_of_a_missing_package_mentions_other_sources() {
        let declarations = [plain(channel("git"))];
        let request = PackageRequest::resolve(&name("pkgs.git"), &[unstable()]);
        assert_eq!(
            find_declaration(&declarations, &request),
            DeclarationMatch::None {
                elsewhere: vec![&declarations[0]]
            }
        );
        let request = PackageRequest::resolve(&name("vim"), &[unstable()]);
        assert_eq!(
            find_declaration(&declarations, &request),
            DeclarationMatch::None {
                elsewhere: Vec::new()
            }
        );
    }

    #[test]
    fn listing_removes_repeated_identities() {
        let declarations = [
            plain(PackageIdentity::nixpkgs("git")),
            plain(channel("git")),
            plain(PackageIdentity::nixpkgs("git")),
        ];
        assert_eq!(unique_declarations(&declarations).len(), 2);
    }

    #[test]
    fn exact_attribute_wins_over_similar_packages() {
        let results = vec![
            package("git-lfs", "git-lfs", ""),
            package("git", "git", ""),
            package("gitFull", "git", ""),
        ];
        assert_eq!(
            resolve_results(results, "git"),
            Resolution::Found(package("git", "git", ""))
        );
    }

    #[test]
    fn description_match_does_not_confirm_a_package() {
        let results = vec![package("repgrep", "repgrep", "Replacer for ripgrep")];
        assert_eq!(resolve_results(results, "ripgrep"), Resolution::NotFound);
    }

    #[test]
    fn single_exact_name_is_accepted() {
        let by_segment = vec![package("kdePackages.kate", "kate", "")];
        assert_eq!(
            resolve_results(by_segment.clone(), "kate"),
            Resolution::Found(by_segment[0].clone())
        );
        let by_pname = vec![package("gtk3", "gtk+3", ""), package("other", "x", "gtk+3")];
        assert_eq!(
            resolve_results(by_pname, "GTK+3"),
            Resolution::Found(package("gtk3", "gtk+3", ""))
        );
    }

    #[test]
    fn several_exact_names_need_a_choice() {
        let results = vec![
            package("qt6Packages.foo", "foo", ""),
            package("qt5Packages.foo", "foo", ""),
        ];
        assert_eq!(
            attrs(resolve_results(results, "foo")),
            ["qt5Packages.foo", "qt6Packages.foo"]
        );
    }

    #[test]
    fn a_similar_package_always_needs_a_choice() {
        let results = vec![package("ripgrep", "ripgrep", "")];
        assert_eq!(attrs(resolve_results(results, "ripgre")), ["ripgrep"]);
    }

    #[test]
    fn search_ranking_keeps_description_matches_last() {
        let results = vec![
            package("other", "other", "Uses foo a lot"),
            package("foobar", "foobar", ""),
            package("foo", "foo", ""),
        ];
        let ranked: Vec<String> = rank_results(results, "foo")
            .into_iter()
            .map(|package| package.identity.attr)
            .collect();
        assert_eq!(ranked, ["foo", "foobar", "other"]);
    }

    #[test]
    fn near_names_find_typos_and_containing_names() {
        let names: Vec<String> = ["ripgrep", "ripgrep-all", "firefox", "grep", "rip"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(near_names(&names, "ripgre", 10), ["ripgrep", "ripgrep-all"]);
        assert_eq!(near_names(&names, "firefx", 10), ["firefox"]);
        assert!(near_names(&names, "zzzzzzzz", 10).is_empty());
        assert_eq!(near_names(&names, "ripgre", 1), ["ripgrep"]);
    }

    #[test]
    fn likely_typos_ignore_real_attributes_and_partial_names() {
        let names: Vec<String> = ["firefox", "firefox-esr", "git"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(likely_typos(&names, "firefx", 5), ["firefox"]);
        assert!(likely_typos(&names, "firefox", 5).is_empty());
        assert!(likely_typos(&names, "fire", 5).is_empty());
        assert!(likely_typos(&names, "web browser", 5).is_empty());
    }

    #[test]
    fn edit_distance_counts_edits() {
        assert_eq!(edit_distance("firefox", "firefox"), 0);
        assert_eq!(edit_distance("firefox", "firefx"), 1);
        assert_eq!(edit_distance("firefox", "fierfox"), 2);
        assert_eq!(edit_distance("a", "abcdef"), MAX_TYPO_DISTANCE + 1);
    }
}
