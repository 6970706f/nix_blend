//! Package models.

use std::fmt;

const PKGS: &str = "pkgs";
const KEYWORDS: [&str; 10] = [
    "assert", "else", "if", "in", "inherit", "let", "or", "rec", "then", "with",
];

/// A name that is not a valid attribute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidPackageName(pub String);

/// Validated attribute path, as typed by the user. Since it can be written to a
/// Nix file, only identifiers separated by dots are accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageName(String);

impl PackageName {
    /// Validates `input`.
    pub fn parse(input: &str) -> Result<Self, InvalidPackageName> {
        if input != PKGS && input.split('.').all(is_valid_identifier) {
            Ok(Self(input.to_owned()))
        } else {
            Err(InvalidPackageName(input.to_owned()))
        }
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn is_valid_identifier(part: &str) -> bool {
    let mut bytes = part.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'\'' | b'-'))
        && !KEYWORDS.contains(&part)
}

/// Package set a package comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PackageSource {
    /// The system's nixpkgs, through `pkgs`.
    Nixpkgs,
    /// Another package set imported by the file, as in
    /// `unstable = import inputs.nixpkgs-unstable { ... };`.
    Channel { alias: String, input: String },
    /// A package taken directly from a flake input, as in `inputs.foo.packages...`.
    FlakeInput(String),
    /// A name the file binds in a way that cannot be traced.
    Unknown,
}

impl PackageSource {
    /// Short name of the source.
    pub fn label(&self) -> &str {
        match self {
            Self::Nixpkgs => "nixpkgs",
            Self::Channel { alias, .. } => alias,
            Self::FlakeInput(input) => input,
            Self::Unknown => "unknown",
        }
    }
}

/// What makes two declarations the same package: its name and where it comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageIdentity {
    pub source: PackageSource,
    pub attr: String,
}

impl PackageIdentity {
    /// Identity of a package in the system's nixpkgs.
    pub fn nixpkgs(attr: impl Into<String>) -> Self {
        Self {
            source: PackageSource::Nixpkgs,
            attr: attr.into(),
        }
    }

    /// Name that identifies the package to the user, including its source when
    /// that is another package set.
    pub fn qualified(&self) -> String {
        match &self.source {
            PackageSource::Channel { alias, .. } => format!("{alias}.{}", self.attr),
            _ => self.attr.clone(),
        }
    }
}

impl fmt::Display for PackageIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.qualified())
    }
}

/// Import of another package set found in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelAlias {
    pub name: String,
    pub input: String,
}

/// Form of an entry in the package list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationKind {
    /// A plain attribute: `git`, `pkgs.git`, `unstable.git`.
    Plain,
    /// The package with changes: `(git.override { })`.
    Override,
    /// An expression based on an attribute: `(python3.withPackages (ps: [ ]))`.
    Expression,
    /// Anything else, which has no package name.
    Opaque,
}

/// One entry of the package list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDeclaration {
    pub identity: Option<PackageIdentity>,
    pub kind: DeclarationKind,
    /// The entry as written in the file.
    pub text: String,
    /// 1-based line where the entry starts.
    pub line: usize,
    /// Byte range of the entry in the file.
    pub start: usize,
    pub end: usize,
}

/// Declarations of a file and the package sets it imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredPackages {
    pub declarations: Vec<PackageDeclaration>,
    pub aliases: Vec<ChannelAlias>,
}

/// Package metadata. A field is `None` when the source did not provide it, and is
/// never filled with a guess.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageMetadata {
    pub pname: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub unfree: Option<bool>,
    pub broken: Option<bool>,
    pub insecure: Option<bool>,
    /// Whether the system configuration accepts the package.
    pub available: Option<bool>,
    /// Name of the package's store path, without the hash.
    pub store_name: Option<String>,
}

/// Why the system configuration refuses a package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocker {
    Broken,
    Unfree,
    Insecure,
    Platform,
}

/// A package offered by a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub identity: PackageIdentity,
    pub metadata: PackageMetadata,
}

impl Package {
    /// What blocks the package, if the configuration refuses it.
    pub fn blockers(&self) -> Vec<Blocker> {
        if self.metadata.available != Some(false) {
            return Vec::new();
        }
        let flags = [
            (self.metadata.broken, Blocker::Broken),
            (self.metadata.unfree, Blocker::Unfree),
            (self.metadata.insecure, Blocker::Insecure),
        ];
        let blockers: Vec<Blocker> = flags
            .into_iter()
            .filter_map(|(flag, blocker)| (flag == Some(true)).then_some(blocker))
            .collect();
        if blockers.is_empty() {
            vec![Blocker::Platform]
        } else {
            blockers
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_attribute_paths() {
        for name in [
            "git",
            "kdePackages.kate",
            "pkgs.yt-dlp",
            "g_2'",
            "unstable.foo",
        ] {
            assert_eq!(PackageName::parse(name).unwrap().as_str(), name);
        }
    }

    #[test]
    fn rejects_names_that_are_not_attribute_paths() {
        for input in [
            "", " ", "a b", "a;b", "a]", "a.", ".a", "a..b", "1a", "-a", "\"a\"", "${a}", "a\nb",
            "with", "a.let", "pkgs", "pkgs.", "a#b", "../a", "é",
        ] {
            assert!(PackageName::parse(input).is_err(), "{input:?}");
        }
    }

    #[test]
    fn qualified_names_include_only_other_package_sets() {
        assert_eq!(PackageIdentity::nixpkgs("git").qualified(), "git");
        let channel = PackageIdentity {
            source: PackageSource::Channel {
                alias: "unstable".to_owned(),
                input: "nixpkgs-unstable".to_owned(),
            },
            attr: "git".to_owned(),
        };
        assert_eq!(channel.qualified(), "unstable.git");
        assert_eq!(channel.source.label(), "unstable");
    }

    fn package(metadata: PackageMetadata) -> Package {
        Package {
            identity: PackageIdentity::nixpkgs("x"),
            metadata,
        }
    }

    #[test]
    fn blockers_only_apply_to_refused_packages() {
        let allowed_unfree = PackageMetadata {
            available: Some(true),
            unfree: Some(true),
            ..PackageMetadata::default()
        };
        assert!(package(allowed_unfree).blockers().is_empty());
        let unknown = PackageMetadata {
            unfree: Some(true),
            ..PackageMetadata::default()
        };
        assert!(package(unknown).blockers().is_empty());
    }

    #[test]
    fn blockers_name_the_reasons() {
        let refused = PackageMetadata {
            available: Some(false),
            broken: Some(true),
            unfree: Some(true),
            ..PackageMetadata::default()
        };
        assert_eq!(
            package(refused).blockers(),
            [Blocker::Broken, Blocker::Unfree]
        );
        let platform = PackageMetadata {
            available: Some(false),
            ..PackageMetadata::default()
        };
        assert_eq!(package(platform).blockers(), [Blocker::Platform]);
    }
}
