//! Package catalog backed by the `nix` command.
//!
//! Evaluating all of nixpkgs takes gigabytes of memory the first time for each
//! revision, so only an explicit search does it. Exact attributes, metadata and the list
//! of attribute names evaluate just what they need, and results go through the Nix
//! evaluation cache. Every query uses the revision locked by the system flake.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::process::{self, ProcessError};
use super::tools;
use super::user::UserIdentity;
use crate::application::error::SearchError;
use crate::application::ports::{CatalogProvider, PackageCatalog, ProgressReporter};
use crate::domain::package::{
    Package, PackageIdentity, PackageMetadata, PackageName, PackageSource,
};

const NIX: &str = "nix";
const SYSTEM_FLAKE: &str = "/etc/nixos";
const SYSTEM_PKGS: &str = "nixosConfigurations.nixos.pkgs";
const NIXPKGS_INPUT: &str = "nixpkgs";
/// Flake registry entry, used when the system's revision cannot be determined.
const REGISTRY_REF: &str = "nixpkgs";
/// Forges whose locked inputs map directly to `type:owner/repo/rev`.
const FORGE_TYPES: [&str; 3] = ["github", "gitlab", "sourcehut"];
const QUERY_TIMEOUT: Duration = Duration::from_secs(180);
/// A full search may need to download nixpkgs and evaluate all of it.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(600);

/// Creates the catalogs of the sources Nix can query.
pub struct NixCatalogs<'a> {
    pub user: UserIdentity,
    pub elevated: bool,
    pub progress: &'a dyn ProgressReporter,
}

impl CatalogProvider for NixCatalogs<'_> {
    fn catalog<'a>(
        &'a self,
        source: &PackageSource,
    ) -> Result<Box<dyn PackageCatalog + 'a>, SearchError> {
        let lock = read_lock(Path::new(SYSTEM_FLAKE));
        let (flake_ref, pinned, system_flake) = match source {
            PackageSource::Nixpkgs => {
                let locked = lock.and_then(|lock| locked_input_ref(&lock, NIXPKGS_INPUT));
                let flake_exists = Path::new(SYSTEM_FLAKE).join("flake.nix").exists();
                (
                    locked.clone().unwrap_or_else(|| REGISTRY_REF.to_owned()),
                    locked.is_some(),
                    flake_exists.then(|| SYSTEM_FLAKE.to_owned()),
                )
            }
            // Such a package set has its own settings, so the system configuration says
            // nothing about it.
            PackageSource::Channel { input, .. } => {
                let locked = lock
                    .and_then(|lock| locked_input_ref(&lock, input))
                    .ok_or_else(|| SearchError::UnknownInput(input.clone()))?;
                (locked, true, None)
            }
            PackageSource::FlakeInput(_) | PackageSource::Unknown => {
                return Err(SearchError::UnsearchableSource);
            }
        };
        Ok(Box::new(NixCatalog {
            program: tools::program(NIX, self.elevated).map_err(SearchError::Failed)?,
            source: source.clone(),
            flake_ref,
            pinned,
            system_flake,
            elevated: self.elevated,
            run_as: (self.elevated && self.user.uid != 0).then(|| self.user.clone()),
            progress: self.progress,
        }))
    }
}

/// `nix` commands against one flake reference.
struct NixCatalog<'a> {
    program: PathBuf,
    source: PackageSource,
    flake_ref: String,
    pinned: bool,
    /// Flake whose system configuration can be evaluated.
    system_flake: Option<String>,
    elevated: bool,
    run_as: Option<UserIdentity>,
    progress: &'a dyn ProgressReporter,
}

impl NixCatalog<'_> {
    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(["--extra-experimental-features", "nix-command flakes"])
            .args(arguments)
            .stdin(Stdio::null());
        tools::apply_environment(&mut command, self.elevated, self.run_as.as_ref());
        command
    }

    fn run(
        &self,
        mut command: Command,
        label: &str,
        timeout: Duration,
    ) -> Result<Output, SearchError> {
        let result = process::run(&mut command, timeout, &mut |seconds| {
            self.progress.update(label, seconds);
        });
        self.progress.finish();
        result.map_err(|error| match error {
            ProcessError::NotFound => SearchError::ToolUnavailable,
            ProcessError::Spawn(error) => SearchError::Spawn(error),
            ProcessError::TimedOut(limit) => SearchError::TimedOut(limit),
        })
    }

    /// The package set evaluated for metadata: the one the system configuration really
    /// uses, with its overlays and settings, when there is one.
    fn package_set(&self) -> String {
        match &self.system_flake {
            Some(flake) => format!("{flake}#{SYSTEM_PKGS}"),
            None => format!("{}#legacyPackages.{}", self.flake_ref, current_system()),
        }
    }

    fn evaluate(&self, expression: &str, label: &str) -> Result<Vec<u8>, SearchError> {
        let package_set = self.package_set();
        let command = self.command(&[
            "eval",
            "--json",
            "--no-write-lock-file",
            &package_set,
            "--apply",
            expression,
        ]);
        let output = self.run(command, label, QUERY_TIMEOUT)?;
        if !output.status.success() {
            return Err(SearchError::Failed(error_message(&output.stderr)));
        }
        Ok(output.stdout)
    }

    /// Without the system configuration, availability would reflect the default
    /// settings of nixpkgs rather than the user's.
    fn parse_metadata(&self, json: &[u8]) -> Result<BTreeMap<String, Package>, SearchError> {
        parse_metadata(json, &self.source, self.system_flake.is_some())
    }

    /// Asks `nix search` about one attribute, without evaluating anything else.
    fn scoped_search(&self, attr: &str) -> Result<Option<Package>, SearchError> {
        let installable = format!("{}#{attr}", self.flake_ref);
        let command = self.command(&["search", "--json", &installable, "^"]);
        let output = self.run(command, "Checking nixpkgs", QUERY_TIMEOUT)?;
        if !output.status.success() {
            return Err(SearchError::Failed(error_message(&output.stderr)));
        }
        let results = parse_results(&output.stdout, &self.source)?;
        Ok(results
            .into_iter()
            .find(|package| package.identity.attr == attr))
    }
}

impl PackageCatalog for NixCatalog<'_> {
    fn exact(&self, attr: &str) -> Result<Option<Package>, SearchError> {
        match self.metadata(&[attr.to_owned()]) {
            Ok(mut packages) => Ok(packages.remove(attr)),
            // The evaluation can fail while the package exists, as with a configuration
            // that is broken right now. If this also fails, nothing is known.
            Err(evaluation_error) => self.scoped_search(attr).map_err(|_| evaluation_error),
        }
    }

    fn names(&self) -> Result<Vec<String>, SearchError> {
        let json = self.evaluate("builtins.attrNames", "Reading package names")?;
        serde_json::from_slice(&json)
            .map_err(|error| SearchError::InvalidResponse(error.to_string()))
    }

    fn search(&self, term: &str) -> Result<Vec<Package>, SearchError> {
        let regex = escape_regex(term);
        let command = self.command(&["search", "--json", &self.flake_ref, &regex]);
        let output = self.run(command, "Searching nixpkgs", SEARCH_TIMEOUT)?;
        if !output.status.success() {
            return Err(SearchError::Failed(error_message(&output.stderr)));
        }
        parse_results(&output.stdout, &self.source)
    }

    fn metadata(&self, attrs: &[String]) -> Result<BTreeMap<String, Package>, SearchError> {
        let expression = metadata_expression(attrs);
        let json = self.evaluate(&expression, "Reading package details")?;
        self.parse_metadata(&json)
    }

    fn is_pinned(&self) -> bool {
        self.pinned
    }
}

fn current_system() -> String {
    format!("{}-linux", std::env::consts::ARCH)
}

/// Builds the expression that reports the metadata of `attrs`. Attributes that throw,
/// such as removed aliases, and attributes that are not derivations come out as null.
/// Names that are not valid attribute paths are left out, since they would become code.
fn metadata_expression(attrs: &[String]) -> String {
    let entries: String = attrs
        .iter()
        .filter(|attr| PackageName::parse(attr).is_ok())
        .map(|attr| format!("\"{attr}\" = report (p.{attr} or null);\n"))
        .collect();
    format!(
        r#"p: let
  license = l: if builtins.isList l then map license l
    else if builtins.isAttrs l then (l.spdxId or l.shortName or l.fullName or null)
    else l;
  safe = x: let r = builtins.tryEval (builtins.deepSeq x x); in if r.success then r.value else null;
  report = t0: let
    r = builtins.tryEval t0;
    t = if r.success then r.value else null;
  in if builtins.isAttrs t && (t.type or null) == "derivation" then {{
    name = safe (t.name or null);
    pname = safe (t.pname or null);
    version = safe (t.version or null);
    description = safe (t.meta.description or null);
    homepage = safe (t.meta.homepage or null);
    license = safe (license (t.meta.license or null));
    available = safe (t.meta.available or null);
    broken = safe (t.meta.broken or null);
    unfree = safe (t.meta.unfree or null);
    insecure = safe (t.meta.insecure or null);
  }} else null;
in {{
{entries}}}"#
    )
}

fn read_lock(flake: &Path) -> Option<String> {
    fs::read_to_string(flake.join("flake.lock")).ok()
}

/// `nix search` takes a regular expression. Validated package names can only contain
/// `.` as a metacharacter, but a search term can contain any of them.
fn escape_regex(term: &str) -> String {
    let mut escaped = String::with_capacity(term.len() + 2);
    for character in term.chars() {
        if r"\.+*?()|[]{}^$".contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Keeps what Nix reported from its first error on, without the progress lines.
fn error_message(stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let start = stderr.find("error:").unwrap_or(0);
    stderr[start..].trim().to_owned()
}

#[derive(Debug, Deserialize)]
struct RawResult {
    pname: Option<String>,
    version: Option<String>,
    description: Option<String>,
}

fn parse_results(json: &[u8], source: &PackageSource) -> Result<Vec<Package>, SearchError> {
    let results: BTreeMap<String, RawResult> = serde_json::from_slice(json)
        .map_err(|error| SearchError::InvalidResponse(error.to_string()))?;
    Ok(results
        .into_iter()
        .filter_map(|(key, raw)| {
            Some(Package {
                identity: PackageIdentity {
                    source: source.clone(),
                    attr: package_attr(&key)?.to_owned(),
                },
                metadata: PackageMetadata {
                    pname: raw.pname.filter(|text| !text.is_empty()),
                    version: raw.version.filter(|text| !text.is_empty()),
                    description: raw.description.filter(|text| !text.is_empty()),
                    ..PackageMetadata::default()
                },
            })
        })
        .collect())
}

#[derive(Debug, Deserialize)]
struct RawMetadata {
    name: Option<String>,
    pname: Option<String>,
    version: Option<String>,
    description: Option<String>,
    homepage: Option<Value>,
    license: Option<Value>,
    available: Option<bool>,
    broken: Option<bool>,
    unfree: Option<bool>,
    insecure: Option<bool>,
}

fn parse_metadata(
    json: &[u8],
    source: &PackageSource,
    availability_is_known: bool,
) -> Result<BTreeMap<String, Package>, SearchError> {
    let raw: BTreeMap<String, Option<RawMetadata>> = serde_json::from_slice(json)
        .map_err(|error| SearchError::InvalidResponse(error.to_string()))?;
    Ok(raw
        .into_iter()
        .filter_map(|(attr, metadata)| {
            let metadata = metadata?;
            let package = Package {
                identity: PackageIdentity {
                    source: source.clone(),
                    attr: attr.clone(),
                },
                metadata: PackageMetadata {
                    pname: metadata.pname.filter(|text| !text.is_empty()),
                    version: metadata.version.filter(|text| !text.is_empty()),
                    description: metadata.description.filter(|text| !text.is_empty()),
                    homepage: metadata.homepage.as_ref().and_then(first_text),
                    license: metadata.license.as_ref().and_then(joined_text),
                    unfree: metadata.unfree,
                    broken: metadata.broken,
                    insecure: metadata.insecure,
                    available: metadata.available.filter(|_| availability_is_known),
                    store_name: metadata.name,
                },
            };
            Some((attr, package))
        })
        .collect())
}

fn first_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => items.iter().find_map(first_text),
        _ => None,
    }
}

fn joined_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(joined_text).collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        _ => None,
    }
}

/// Strips `legacyPackages.<system>.` or `packages.<system>.` from a result key.
fn package_attr(key: &str) -> Option<&str> {
    let rest = key
        .strip_prefix("legacyPackages.")
        .or_else(|| key.strip_prefix("packages."))?;
    let (_system, attr) = rest.split_once('.')?;
    Some(attr)
}

/// Flake reference of the revision locked for a root input, if the lock file has one in
/// a form that can be expressed as a reference.
fn locked_input_ref(lock: &str, input: &str) -> Option<String> {
    let lock: Value = serde_json::from_str(lock).ok()?;
    let nodes = lock.get("nodes")?;
    let root = nodes.get(lock.get("root")?.as_str()?)?;
    let node = root.get("inputs")?.get(input)?.as_str()?;
    let locked = nodes.get(node)?.get("locked")?;

    let field = |name: &str| locked.get(name)?.as_str();
    let forge = field("type").filter(|forge| FORGE_TYPES.contains(forge))?;
    if field("host").is_some() {
        return None;
    }
    Some(format!(
        "{forge}:{}/{}/{}",
        field("owner")?,
        field("repo")?,
        field("rev")?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESULTS: &str = r#"{
        "legacyPackages.x86_64-linux.ripgrep": {
            "pname": "ripgrep", "version": "15.1.0", "description": "Fast grep"
        },
        "legacyPackages.x86_64-linux.kdePackages.kate": {
            "pname": "kate", "version": "26.04.3", "description": ""
        },
        "packages.x86_64-linux.tool": { "pname": "tool", "version": "1" },
        "lib.something": { "pname": "ignored", "version": "1", "description": "" }
    }"#;

    const METADATA: &str = r#"{
        "firefox": {
            "name": "firefox-158", "pname": "firefox", "version": "158",
            "description": "Web browser", "homepage": "http://mozilla.com",
            "license": "MPL-2.0", "available": true, "broken": false,
            "unfree": false, "insecure": false
        },
        "kate": {
            "name": "kate-1", "pname": "kate", "version": "1", "description": null,
            "homepage": ["https://a", "https://b"], "license": ["GPL-2.0", ["MIT"], null],
            "available": false, "broken": false, "unfree": true, "insecure": false
        },
        "lib": null
    }"#;

    const LOCK: &str = r#"{
        "nodes": {
            "nixpkgs_2": {
                "locked": { "owner": "NixOS", "repo": "nixpkgs", "rev": "718f670e", "type": "github" }
            },
            "unstable": {
                "locked": { "owner": "NixOS", "repo": "nixpkgs", "rev": "31ecbaca", "type": "github" }
            },
            "root": { "inputs": { "nixpkgs": "nixpkgs_2", "nixpkgs-unstable": "unstable" } }
        },
        "root": "root",
        "version": 7
    }"#;

    struct NoProgress;
    impl ProgressReporter for NoProgress {
        fn update(&self, _label: &str, _seconds: u64) {}
        fn finish(&self) {}
    }

    fn catalog(program: &str, system_flake: Option<&str>) -> NixCatalog<'static> {
        NixCatalog {
            program: PathBuf::from(program),
            source: PackageSource::Nixpkgs,
            flake_ref: "github:NixOS/nixpkgs/abc".to_owned(),
            pinned: true,
            system_flake: system_flake.map(str::to_owned),
            elevated: false,
            run_as: None,
            progress: &NoProgress,
        }
    }

    #[test]
    fn parses_results_and_strips_the_output_prefix() {
        let results = parse_results(RESULTS.as_bytes(), &PackageSource::Nixpkgs).unwrap();
        let attrs: Vec<&str> = results
            .iter()
            .map(|package| package.identity.attr.as_str())
            .collect();
        assert_eq!(attrs, ["kdePackages.kate", "ripgrep", "tool"]);
        assert_eq!(results[1].metadata.version.as_deref(), Some("15.1.0"));
        assert_eq!(
            results[1].metadata.description.as_deref(),
            Some("Fast grep")
        );
        assert_eq!(results[0].metadata.description, None);
        assert_eq!(results[2].metadata.description, None);
        assert_eq!(results[1].metadata.license, None);
    }

    #[test]
    fn empty_object_means_no_results() {
        assert!(
            parse_results(b"{}", &PackageSource::Nixpkgs)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn invalid_json_is_an_error_and_not_an_empty_result() {
        for json in ["", "not json", "[]", r#"{"a": 1}"#] {
            assert!(
                matches!(
                    parse_results(json.as_bytes(), &PackageSource::Nixpkgs),
                    Err(SearchError::InvalidResponse(_))
                ),
                "{json}"
            );
            assert!(matches!(
                parse_metadata(json.as_bytes(), &PackageSource::Nixpkgs, true),
                Err(SearchError::InvalidResponse(_))
            ));
        }
    }

    #[test]
    fn metadata_keeps_what_is_known_and_leaves_the_rest_unknown() {
        let packages = parse_metadata(METADATA.as_bytes(), &PackageSource::Nixpkgs, true).unwrap();
        assert!(!packages.contains_key("lib"));

        let firefox = &packages["firefox"].metadata;
        assert_eq!(firefox.license.as_deref(), Some("MPL-2.0"));
        assert_eq!(firefox.homepage.as_deref(), Some("http://mozilla.com"));
        assert_eq!(firefox.available, Some(true));
        assert_eq!(firefox.store_name.as_deref(), Some("firefox-158"));

        let kate = &packages["kate"].metadata;
        assert_eq!(kate.description, None);
        assert_eq!(kate.license.as_deref(), Some("GPL-2.0, MIT"));
        assert_eq!(kate.homepage.as_deref(), Some("https://a"));
        assert_eq!(kate.unfree, Some(true));
    }

    #[test]
    fn availability_needs_the_system_configuration() {
        let packages = parse_metadata(METADATA.as_bytes(), &PackageSource::Nixpkgs, false).unwrap();
        assert_eq!(packages["kate"].metadata.available, None);
        assert_eq!(packages["kate"].metadata.unfree, Some(true));
    }

    #[test]
    fn metadata_expression_leaves_out_what_would_become_code() {
        let attrs = [
            "kdePackages.kate".to_owned(),
            "x; }; injected".to_owned(),
            "${x}".to_owned(),
        ];
        let expression = metadata_expression(&attrs);
        assert!(expression.contains("\"kdePackages.kate\" = report (p.kdePackages.kate or null);"));
        assert!(!expression.contains("injected"));
        assert!(!expression.contains("${x}"));
    }

    #[test]
    fn resolves_locked_revisions_through_the_root_inputs() {
        assert_eq!(
            locked_input_ref(LOCK, "nixpkgs").as_deref(),
            Some("github:NixOS/nixpkgs/718f670e")
        );
        assert_eq!(
            locked_input_ref(LOCK, "nixpkgs-unstable").as_deref(),
            Some("github:NixOS/nixpkgs/31ecbaca")
        );
        assert_eq!(locked_input_ref(LOCK, "other"), None);
    }

    #[test]
    fn unusable_locks_have_no_reference() {
        for lock in [
            "not json",
            "{}",
            r#"{"nodes":{"root":{"inputs":{}}},"root":"root"}"#,
            r#"{"nodes":{"root":{"inputs":{"nixpkgs":["a","b"]}}},"root":"root"}"#,
            r#"{"nodes":{"n":{"locked":{"type":"path","path":"/x"}},"root":{"inputs":{"nixpkgs":"n"}}},"root":"root"}"#,
            r#"{"nodes":{"n":{"locked":{"type":"gitlab","host":"x.y","owner":"o","repo":"r","rev":"1"}},"root":{"inputs":{"nixpkgs":"n"}}},"root":"root"}"#,
        ] {
            assert_eq!(locked_input_ref(lock, "nixpkgs"), None, "{lock}");
        }
    }

    #[test]
    fn metadata_comes_from_the_system_configuration_when_there_is_one() {
        assert_eq!(
            catalog("nix", Some("/etc/nixos")).package_set(),
            "/etc/nixos#nixosConfigurations.nixos.pkgs"
        );
        assert_eq!(
            catalog("nix", None).package_set(),
            format!(
                "github:NixOS/nixpkgs/abc#legacyPackages.{}",
                current_system()
            )
        );
    }

    #[test]
    fn escapes_regex_metacharacters() {
        assert_eq!(escape_regex("kdePackages.kate"), r"kdePackages\.kate");
        assert_eq!(escape_regex("yt-dlp"), "yt-dlp");
        assert_eq!(escape_regex("a+(b)"), r"a\+\(b\)");
    }

    #[test]
    fn error_message_skips_progress_output() {
        let stderr = b"evaluating 'x'...\nerror: unable to download 'y'\n";
        assert_eq!(error_message(stderr), "error: unable to download 'y'");
        assert_eq!(error_message(b" other \n"), "other");
    }

    #[test]
    fn failures_are_errors_and_never_missing_packages() {
        let failing = catalog("false", None);
        assert!(matches!(failing.exact("git"), Err(SearchError::Failed(_))));
        assert!(matches!(failing.search("git"), Err(SearchError::Failed(_))));
        assert!(matches!(failing.names(), Err(SearchError::Failed(_))));
    }

    #[test]
    fn missing_tool_is_an_error_for_every_operation() {
        let missing = catalog("/nonexistent/nix", None);
        assert!(matches!(
            missing.exact("git"),
            Err(SearchError::ToolUnavailable)
        ));
        assert!(matches!(
            missing.search("git"),
            Err(SearchError::ToolUnavailable)
        ));
        assert!(matches!(missing.names(), Err(SearchError::ToolUnavailable)));
    }

    #[test]
    fn successful_exit_with_garbage_is_an_error() {
        assert!(matches!(
            catalog("true", None).search("git"),
            Err(SearchError::InvalidResponse(_))
        ));
    }
}
