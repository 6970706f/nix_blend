//! Full-flow tests, with external tools simulated by scripts in an isolated
//! `PATH`. No test touches the real system configuration.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;

use tempfile::TempDir;

/// Serializes script creation and process startup: a concurrent `fork` could
/// inherit a script open for writing and cause `ETXTBSY` on execution.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

const MODULE: &str = "{ pkgs, ... }:\n{\n  home.packages = with pkgs; [\n    git\n  ];\n}\n";
const MODULE_WITH_FOO: &str =
    "{ pkgs, ... }:\n{\n  home.packages = with pkgs; [\n    git\n    foo\n  ];\n}\n";

const FOO_METADATA: &str = r#"{"foo":{"name":"foo-1.0","pname":"foo","version":"1.0","description":"Foo tool","homepage":"https://foo.example","license":"MIT","available":true,"broken":false,"unfree":false,"insecure":false}}"#;

/// Simulates `nix`: `names` answers the list of attributes, `metadata` the evaluation of
/// attributes, and `search` a full search.
fn nix_answering(names: &str, metadata: &str, search: &str) -> String {
    format!(
        r#"case "$*" in
  *attrNames*) echo '{names}';;
  *" eval "*) echo '{metadata}';;
  *) echo '{search}';;
esac"#
    )
}

fn nix_finding_foo() -> String {
    nix_answering(r#"["foo"]"#, FOO_METADATA, "{}")
}

struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        // As root, the binary ignores `PATH` and would run the real system tools,
        // including `nixos-rebuild switch`.
        assert!(
            !rustix::process::geteuid().is_root(),
            "these tests must not run as root"
        );
        let sandbox = Self {
            root: tempfile::tempdir().unwrap(),
        };
        fs::create_dir(sandbox.bin()).unwrap();
        sandbox.tool("nix", &nix_finding_foo());
        sandbox.tool("nix-instantiate", "exit 0");
        sandbox.tool("nixos-rebuild", &sandbox.logging_tool("nixos-rebuild", 0));
        sandbox.tool("sudo", &sandbox.logging_tool("sudo", 0));
        fs::write(sandbox.packages(), MODULE).unwrap();
        sandbox
    }

    fn bin(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    fn packages(&self) -> PathBuf {
        self.root.path().join("packages.nix")
    }

    fn data_home(&self) -> PathBuf {
        self.root.path().join("data")
    }

    fn config(&self) -> PathBuf {
        self.data_home().join("nixblend/config.toml")
    }

    fn log(&self) -> PathBuf {
        self.root.path().join("commands.log")
    }

    fn logging_tool(&self, name: &str, status: i32) -> String {
        format!(
            "echo {name} \"$@\" >> '{}'\nexit {status}",
            self.log().display()
        )
    }

    fn tool(&self, name: &str, body: &str) {
        let _lock = SPAWN_LOCK.lock().unwrap();
        let path = self.bin().join(name);
        let shell = which("sh");
        fs::write(&path, format!("#!{}\n{body}\n", shell.display())).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn remove_tool(&self, name: &str) {
        fs::remove_file(self.bin().join(name)).unwrap();
    }

    fn write_config(&self) {
        fs::create_dir_all(self.config().parent().unwrap()).unwrap();
        fs::write(
            self.config(),
            format!("packages_file = \"{}\"\n", self.packages().display()),
        )
        .unwrap();
    }

    fn run(&self, package: &str, input: &str) -> Output {
        let mut child = {
            let _lock = SPAWN_LOCK.lock().unwrap();
            Command::new(env!("CARGO_BIN_EXE_nixblend"))
                .args(["install", package])
                .env("PATH", self.bin())
                .env("XDG_DATA_HOME", self.data_home())
                .env_remove("SUDO_USER")
                .env_remove("SUDO_UID")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        };
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn commands(&self) -> String {
        fs::read_to_string(self.log()).unwrap_or_default()
    }

    fn content(&self) -> String {
        fs::read_to_string(self.packages()).unwrap()
    }

    /// Names in the `packages.nix` directory, to detect orphaned temporary files.
    fn leftovers(&self) -> Vec<String> {
        names(self.root.path())
            .into_iter()
            .filter(|name| name.contains(".nixblend-") && !name.ends_with("-backup"))
            .collect()
    }
}

fn names(directory: &Path) -> Vec<String> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

fn which(program: &str) -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join(program))
                .find(|candidate| candidate.is_file())
        })
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn first_run_saves_config_edits_and_rebuilds() {
    let sandbox = Sandbox::new();
    let input = format!("{}\ny\ny\n", sandbox.packages().display());

    let output = sandbox.run("foo", &input);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(sandbox.content(), MODULE_WITH_FOO);
    let config = fs::read_to_string(sandbox.config()).unwrap();
    assert!(config.contains(sandbox.packages().to_str().unwrap()));

    let text = stdout(&output);
    assert!(text.contains("Path to the packages.nix"));
    assert!(text.contains("+    foo\n"));
    assert!(text.contains("Edit: package 'foo' added"));
    assert!(text.contains("Rebuild: done."));
    assert_eq!(
        sandbox.commands(),
        "sudo nixos-rebuild switch --flake /etc/nixos#nixos\n"
    );
    assert!(sandbox.leftovers().is_empty());
}

#[test]
fn second_run_does_not_ask_for_the_path_again() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    let output = sandbox.run("foo", "y\nn\n");

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!stdout(&output).contains("Path to the packages.nix"));
    assert_eq!(sandbox.content(), MODULE_WITH_FOO);
}

#[test]
fn diff_appears_before_the_confirmation_and_refusal_keeps_the_file() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    let output = sandbox.run("foo", "n\n");

    assert!(output.status.success());
    let text = stdout(&output);
    let diff_at = text.find("+    foo").unwrap();
    let prompt_at = text.find("Apply this change?").unwrap();
    assert!(diff_at < prompt_at);
    assert!(text.contains("Edit cancelled"));
    assert!(!text.contains("NixOS rebuild"));
    assert_eq!(sandbox.content(), MODULE);
    assert_eq!(sandbox.commands(), "");
}

#[test]
fn end_of_input_at_confirmation_changes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    let output = sandbox.run("foo", "");

    assert!(output.status.success());
    assert_eq!(sandbox.content(), MODULE);
}

#[test]
fn cancelling_the_rebuild_keeps_the_edit() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    let output = sandbox.run("foo", "y\nn\n");

    assert!(output.status.success());
    assert_eq!(sandbox.content(), MODULE_WITH_FOO);
    assert!(stdout(&output).contains("Rebuild: not run. The edit was kept."));
    assert_eq!(sandbox.commands(), "");
}

#[test]
fn failed_rebuild_is_separate_from_the_edit() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool("sudo", "exit 7");

    let output = sandbox.run("foo", "y\ny\n");

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(sandbox.content(), MODULE_WITH_FOO);
    let text = stdout(&output);
    assert!(text.contains("Edit: package 'foo' added"));
    assert!(text.contains("Rebuild: failed."));
    assert!(stderr(&output).contains("rebuild error"));
}

#[test]
fn missing_rebuild_tool_is_a_rebuild_error() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.remove_tool("nixos-rebuild");
    sandbox.remove_tool("sudo");

    let output = sandbox.run("foo", "y\ny\n");

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(sandbox.content(), MODULE_WITH_FOO);
    assert!(stderr(&output).contains("could not start"));
}

#[test]
fn duplicate_package_changes_nothing_and_asks_nothing() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    let output = sandbox.run("git", "");

    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.contains("already in"));
    assert!(!text.contains("Apply"));
    assert!(!text.contains("rebuild"));
    assert_eq!(sandbox.content(), MODULE);
}

#[test]
fn nix_validation_failure_keeps_the_original_and_skips_the_rebuild() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool("nix-instantiate", "echo 'syntax error' >&2\nexit 1");

    let output = sandbox.run("foo", "y\ny\n");

    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains("edit error"), "{error}");
    assert!(error.contains("syntax error"), "{error}");
    assert_eq!(sandbox.content(), MODULE);
    assert_eq!(sandbox.commands(), "");
    assert!(sandbox.leftovers().is_empty());
}

#[test]
fn missing_nix_tool_warns_and_still_edits() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.remove_tool("nix-instantiate");

    let output = sandbox.run("foo", "y\nn\n");

    assert!(output.status.success());
    assert_eq!(sandbox.content(), MODULE_WITH_FOO);
    assert!(stdout(&output).contains("syntax was not checked"));
}

#[test]
fn unsupported_structure_stops_before_any_prompt() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    let source = "{ pkgs, ... }: { home.packages = [ pkgs.a ] ++ [ pkgs.b ]; }\n";
    fs::write(sandbox.packages(), source).unwrap();

    let output = sandbox.run("foo", "y\ny\n");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("edit error"));
    assert!(!stdout(&output).contains("Apply"));
    assert_eq!(sandbox.content(), source);
}

#[test]
fn invalid_package_name_stops_before_reading_anything() {
    let sandbox = Sandbox::new();

    let output = sandbox.run("foo;bar", "");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("invalid package name"));
    assert!(!sandbox.config().exists());
}

#[test]
fn invalid_first_run_path_saves_no_config() {
    let sandbox = Sandbox::new();

    let output = sandbox.run("foo", "relative/packages.nix\n");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("config error"));
    assert!(!sandbox.config().exists());
}

#[test]
fn missing_configured_file_is_reported() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    fs::remove_file(sandbox.packages()).unwrap();

    let output = sandbox.run("foo", "");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("does not exist"));
}

#[test]
fn corrupt_config_is_not_overwritten() {
    let sandbox = Sandbox::new();
    fs::create_dir_all(sandbox.config().parent().unwrap()).unwrap();
    fs::write(sandbox.config(), "packages_file = [").unwrap();

    let output = sandbox.run("foo", "");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("config error"));
    assert_eq!(
        fs::read_to_string(sandbox.config()).unwrap(),
        "packages_file = ["
    );
}

#[test]
fn unwritable_directory_is_an_edit_error() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    let readonly = sandbox.root.path().join("readonly");
    fs::create_dir(&readonly).unwrap();
    let packages = readonly.join("packages.nix");
    fs::write(&packages, MODULE).unwrap();
    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o500)).unwrap();
    fs::write(
        sandbox.config(),
        format!("packages_file = \"{}\"\n", packages.display()),
    )
    .unwrap();

    let output = sandbox.run("foo", "y\n");

    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no permission"));
    assert_eq!(fs::read_to_string(&packages).unwrap(), MODULE);
}

#[test]
fn missing_package_changes_nothing_and_asks_nothing() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    let output = sandbox.run("nope", "y\ny\n");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("was not found in nixpkgs"));
    assert!(!stdout(&output).contains("Apply"));
    assert_eq!(sandbox.content(), MODULE);
}

#[test]
fn search_failure_is_not_reported_as_a_missing_package() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool(
        "nix",
        "echo \"error: unable to download 'x': Could not resolve hostname\" >&2\nexit 1",
    );

    let output = sandbox.run("foo", "y\ny\n");

    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains("could not validate"), "{error}");
    assert!(error.contains("Could not resolve hostname"), "{error}");
    assert!(!error.contains("was not found"), "{error}");
    assert_eq!(sandbox.content(), MODULE);
}

#[test]
fn missing_nix_is_a_validation_error() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.remove_tool("nix");

    let output = sandbox.run("foo", "y\ny\n");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("could not validate"));
    assert_eq!(sandbox.content(), MODULE);
}

/// No attribute is known, and a full search returns `json`.
fn nix_search_returning(json: &str) -> String {
    nix_answering("[]", "{}", json)
}

const SIMILAR: &str = r#"{"legacyPackages.x86_64-linux.foo-a":{"pname":"foo-a","version":"1.0","description":"First tool"},"legacyPackages.x86_64-linux.foo-b":{"pname":"foo-b","version":"2.0","description":"Second tool"},"legacyPackages.x86_64-linux.other":{"pname":"other","version":"3.0","description":"Mentions foo"}}"#;

#[test]
fn similar_results_are_offered_and_the_choice_is_installed() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool("nix", &nix_search_returning(SIMILAR));

    let output = sandbox.run("foo", "y\n2\ny\nn\n");

    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("1) foo-a (1.0) - First tool"), "{text}");
    assert!(text.contains("2) foo-b (2.0) - Second tool"), "{text}");
    assert!(!text.contains("other (3.0)"), "{text}");
    assert_eq!(
        sandbox.content(),
        MODULE.replace("    git\n", "    git\n    foo-b\n")
    );
}

#[test]
fn cancelling_the_choice_changes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool("nix", &nix_search_returning(SIMILAR));

    let output = sandbox.run("foo", "y\n\n");

    assert!(output.status.success());
    assert!(stdout(&output).contains("Nothing was changed"));
    assert!(!stdout(&output).contains("Apply"));
    assert_eq!(sandbox.content(), MODULE);
}

#[test]
fn description_only_results_mean_the_package_does_not_exist() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    let only_description = r#"{"legacyPackages.x86_64-linux.other":{"pname":"other","version":"3.0","description":"Mentions foo"}}"#;
    sandbox.tool("nix", &nix_search_returning(only_description));

    let output = sandbox.run("foo", "y\n");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("was not found in nixpkgs"));
    assert_eq!(sandbox.content(), MODULE);
}

#[test]
fn chosen_attribute_already_in_the_list_is_a_duplicate() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    let by_pname =
        r#"{"legacyPackages.x86_64-linux.git":{"pname":"foo","version":"1.0","description":""}}"#;
    sandbox.tool("nix", &nix_search_returning(by_pname));

    let output = sandbox.run("foo", "y\n");

    assert!(output.status.success());
    assert!(stdout(&output).contains("Package 'git' is already in"));
    assert_eq!(sandbox.content(), MODULE);
}

#[test]
fn unwritable_file_stops_before_any_search_or_prompt() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool("nix", "echo searched >&2; exit 1");
    fs::set_permissions(sandbox.root.path(), fs::Permissions::from_mode(0o500)).unwrap();

    let output = sandbox.run("foo", "y\n");

    fs::set_permissions(sandbox.root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no permission"));
    assert!(!stderr(&output).contains("searched"));
    assert!(!stdout(&output).contains("Apply"));
}

#[test]
fn unusable_first_run_path_is_not_saved() {
    let sandbox = Sandbox::new();
    let wrong = sandbox.root.path().join("wrong.nix");
    fs::write(&wrong, "{ }").unwrap();

    let output = sandbox.run("foo", &format!("{}\n", wrong.display()));

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no package list"));
    assert!(stdout(&output).contains("The path was not saved"));
    assert!(!sandbox.config().exists());
}

#[test]
fn saved_path_that_stopped_working_points_at_the_config() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    fs::write(sandbox.packages(), "{ }").unwrap();

    let output = sandbox.run("foo", "");

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains(sandbox.config().to_str().unwrap()));
    assert!(stdout(&output).contains("Edit or delete it"));
}

#[test]
fn config_pointing_at_another_kind_of_file_is_refused() {
    let sandbox = Sandbox::new();
    let text = sandbox.root.path().join("notes.txt");
    fs::write(&text, MODULE).unwrap();
    fs::create_dir_all(sandbox.config().parent().unwrap()).unwrap();
    fs::write(
        sandbox.config(),
        format!("packages_file = \"{}\"\n", text.display()),
    )
    .unwrap();

    let output = sandbox.run("foo", "y\nn\n");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("config error"));
    assert_eq!(fs::read_to_string(&text).unwrap(), MODULE);
}

#[test]
fn syntax_failure_is_reported_before_asking_for_confirmation() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool("nix-instantiate", "echo 'syntax error' >&2\nexit 1");

    let output = sandbox.run("foo", "y\n");

    assert_eq!(output.status.code(), Some(1));
    assert!(!stdout(&output).contains("Apply"));
}

#[test]
fn stale_temporary_files_are_removed() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    let stale = sandbox
        .root
        .path()
        .join(".packages.nix.nixblend-4000000000-0");
    fs::write(&stale, "").unwrap();

    let output = sandbox.run("foo", "n\n");

    assert!(output.status.success());
    assert!(!stale.exists());
}

#[test]
fn failed_rebuild_can_undo_the_edit() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool("sudo", "exit 7");

    let output = sandbox.run("foo", "y\ny\ny\n");

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(sandbox.content(), MODULE);
    assert!(stdout(&output).contains("the file is back to its previous content"));
}

#[test]
fn package_from_another_channel_needs_a_decision() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    let source = "{ pkgs, inputs, ... }:\nlet\n  unstable = import inputs.nixpkgs-unstable { };\nin {\n  home.packages = with pkgs; [\n    unstable.foo\n  ];\n}\n";
    fs::write(sandbox.packages(), source).unwrap();

    let output = sandbox.run("foo", "");

    assert!(output.status.success());
    assert!(stdout(&output).contains("declared from another source: unstable.foo"));
    assert!(stdout(&output).contains("as well?"));
    assert_eq!(sandbox.content(), source);
}

#[test]
fn every_similar_package_can_be_reached() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    let entries: Vec<String> = (10..35)
        .map(|index| {
            format!(
                r#""legacyPackages.x86_64-linux.foo{index}":{{"pname":"foo{index}","version":"1","description":"d"}}"#
            )
        })
        .collect();
    let json = format!("{{{}}}", entries.join(","));
    sandbox.tool("nix", &nix_search_returning(&json));

    let output = sandbox.run("foo", "y\nm\n25\ny\nn\n");

    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("m for more"), "{text}");
    assert!(text.contains("25) foo34"), "{text}");
    assert!(sandbox.content().contains("    foo34\n"));
}

impl Sandbox {
    /// Runs any command with the same environment as [`Sandbox::run`].
    fn command(&self, arguments: &[&str], input: &str) -> Output {
        let mut child = {
            let _lock = SPAWN_LOCK.lock().unwrap();
            Command::new(env!("CARGO_BIN_EXE_nixblend"))
                .args(arguments)
                .env("PATH", self.bin())
                .env("XDG_DATA_HOME", self.data_home())
                .env_remove("SUDO_USER")
                .env_remove("SUDO_UID")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        };
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn write_packages(&self, content: &str) {
        fs::write(self.packages(), content).unwrap();
    }

    fn backup(&self) -> PathBuf {
        self.root.path().join(".packages.nix.nixblend-backup")
    }
}

const TWO_SOURCES: &str = "{ pkgs, inputs, ... }:\nlet\n  unstable = import inputs.nixpkgs-unstable { };\nin {\n  home.packages = with pkgs; [\n    # Browsers\n    firefox\n    unstable.firefox\n    git # vcs\n  ];\n}\n";

#[test]
fn edit_keeps_a_backup_of_the_previous_content() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    assert!(sandbox.run("foo", "y\nn\n").status.success());

    assert_eq!(fs::read_to_string(sandbox.backup()).unwrap(), MODULE);
    assert_eq!(sandbox.content(), MODULE_WITH_FOO);
}

#[test]
fn cancelled_edit_leaves_no_backup() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    assert!(sandbox.run("foo", "n\n").status.success());

    assert!(!sandbox.backup().exists());
}

#[test]
fn install_no_rebuild_does_not_ask() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    let output = sandbox.command(&["install", "foo", "--no-rebuild"], "y\ny\n");

    assert!(output.status.success());
    assert!(!stdout(&output).contains("NixOS rebuild"));
    assert_eq!(sandbox.commands(), "");
}

#[test]
fn typo_is_answered_without_a_full_search() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool(
        "nix",
        &nix_answering(r#"["foo","other"]"#, FOO_METADATA, r#"{"never":"#),
    );

    let output = sandbox.run("fooo", "1\ny\nn\n");

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!stdout(&output).contains("Search all package names"));
    assert_eq!(sandbox.content(), MODULE_WITH_FOO);
}

#[test]
fn declining_the_full_search_does_not_claim_the_package_is_missing() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    let output = sandbox.run("nope", "n\n");

    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains("a full search was not run"), "{error}");
    assert!(!error.contains("was not found"), "{error}");
    assert_eq!(sandbox.content(), MODULE);
}

#[test]
fn broken_current_file_is_not_edited() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool("nix-instantiate", "echo 'error: broken' >&2\nexit 1");

    let output = sandbox.run("foo", "y\ny\n");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("the current file is not valid Nix"));
    assert_eq!(sandbox.content(), MODULE);
}

#[test]
fn interruption_leaves_no_temporary_file() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool(
        "nix-instantiate",
        &format!("{} 30", which("sleep").display()),
    );

    let mut child = {
        let _lock = SPAWN_LOCK.lock().unwrap();
        Command::new(env!("CARGO_BIN_EXE_nixblend"))
            .args(["install", "foo"])
            .env("PATH", sandbox.bin())
            .env("XDG_DATA_HOME", sandbox.data_home())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while sandbox.leftovers().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "no temporary file appeared"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let pid = rustix::process::Pid::from_raw(child.id() as i32).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();
    let status = child.wait().unwrap();

    assert!(!status.success());
    assert!(sandbox.leftovers().is_empty(), "{:?}", sandbox.leftovers());
    assert_eq!(sandbox.content(), MODULE);
}

#[test]
fn remove_shows_the_declaration_and_keeps_the_rest() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.write_packages(TWO_SOURCES);

    let output = sandbox.command(&["remove", "git"], "y\nn\n");

    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("Declaration to remove: 'git' (line 9)"),
        "{text}"
    );
    assert!(text.contains("-    git # vcs\n"), "{text}");
    assert_eq!(
        sandbox.content(),
        TWO_SOURCES.replace("    git # vcs\n", "")
    );
    assert_eq!(fs::read_to_string(sandbox.backup()).unwrap(), TWO_SOURCES);
}

#[test]
fn remove_stops_on_an_ambiguous_name() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.write_packages(TWO_SOURCES);

    let output = sandbox.command(&["remove", "firefox"], "y\ny\n");

    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains("'firefox' (line 7)"), "{error}");
    assert!(error.contains("'unstable.firefox' (line 8)"), "{error}");
    assert_eq!(sandbox.content(), TWO_SOURCES);
}

#[test]
fn remove_accepts_a_qualified_name() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.write_packages(TWO_SOURCES);

    let output = sandbox.command(&["remove", "unstable.firefox", "--no-rebuild"], "y\n");

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        sandbox.content(),
        TWO_SOURCES.replace("    unstable.firefox\n", "")
    );
    assert_eq!(sandbox.commands(), "");
}

#[test]
fn remove_of_an_undeclared_package_changes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    let output = sandbox.command(&["remove", "vim"], "y\n");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("is not declared"));
    assert_eq!(sandbox.content(), MODULE);
}

#[test]
fn remove_rebuilds_when_confirmed() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    let output = sandbox.command(&["remove", "git"], "y\ny\n");

    assert!(output.status.success());
    assert_eq!(
        sandbox.commands(),
        "sudo nixos-rebuild switch --flake /etc/nixos#nixos\n"
    );
}

#[test]
fn list_shows_declarations_with_their_sources() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.write_packages(TWO_SOURCES);
    sandbox.tool("nix", "echo must-not-run >&2; exit 1");

    let output = sandbox.command(&["list"], "");

    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("PACKAGE  SOURCE\n"), "{text}");
    assert!(text.contains("firefox  nixpkgs\n"), "{text}");
    assert!(text.contains("firefox  unstable\n"), "{text}");
    assert!(text.contains("git      nixpkgs\n"), "{text}");
    assert_eq!(sandbox.content(), TWO_SOURCES);
}

#[test]
fn list_without_a_configuration_explains_what_to_do() {
    let sandbox = Sandbox::new();

    let output = sandbox.command(&["list"], "/some/path.nix\n");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no managed file is configured"));
    assert!(!sandbox.config().exists());
}

#[test]
fn list_can_compare_with_the_active_generation() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool(
        "nix-store",
        "echo /nix/store/aaa-git-1.0\necho /nix/store/bbb-other-1.0",
    );

    let output = sandbox.command(&["list", "--active"], "");

    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("ACTIVE"), "{text}");
    assert!(text.contains("git      nixpkgs  yes"), "{text}");
}

#[test]
fn info_shows_metadata_and_declaration() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    let output = sandbox.command(&["info", "foo"], "");

    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    for line in [
        "Package: foo",
        "Attribute: foo",
        "Version: 1.0",
        "Description: Foo tool",
        "License: MIT",
        "Source: nixpkgs",
        "Homepage: https://foo.example",
        "Unfree: No",
        "Broken: No",
        "Declared: No",
    ] {
        assert!(text.contains(line), "{line} not in {text}");
    }
}

#[test]
fn info_works_without_a_configuration() {
    let sandbox = Sandbox::new();

    let output = sandbox.command(&["info", "foo"], "");

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("Declared: Unknown"));
    assert!(!sandbox.config().exists());
}

#[test]
fn info_of_a_missing_package_is_an_error() {
    let sandbox = Sandbox::new();

    let output = sandbox.command(&["info", "nope"], "");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("was not found"));
}

#[test]
fn info_failure_is_not_a_missing_package() {
    let sandbox = Sandbox::new();
    sandbox.tool("nix", "echo 'error: offline' >&2\nexit 1");

    let output = sandbox.command(&["info", "foo"], "");

    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains("offline"), "{error}");
    assert!(!error.contains("was not found"), "{error}");
}

#[test]
fn search_shows_results_with_details() {
    let sandbox = Sandbox::new();
    sandbox.tool(
        "nix",
        &nix_answering(
            "[]",
            FOO_METADATA,
            r#"{"legacyPackages.x86_64-linux.foo":{"pname":"foo","version":"1.0","description":"Foo tool"},"legacyPackages.x86_64-linux.bar":{"pname":"bar","version":"2.0","description":"Uses foo"}}"#,
        ),
    );

    let output = sandbox.command(&["search", "foo"], "");

    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.find("Attribute: foo").unwrap() < text.find("Attribute: bar").unwrap());
    assert!(text.contains("License: MIT"), "{text}");
    assert!(text.contains("Status: Available"), "{text}");
    assert!(text.contains("Status: Unknown"), "{text}");
    assert!(text.contains("Showing 2 of 2 result(s)."), "{text}");
}

#[test]
fn search_selection_shows_the_details() {
    let sandbox = Sandbox::new();
    sandbox.tool(
        "nix",
        &nix_answering(
            "[]",
            FOO_METADATA,
            r#"{"legacyPackages.x86_64-linux.foo":{"pname":"foo","version":"1.0","description":"Foo tool"}}"#,
        ),
    );

    let output = sandbox.command(&["search", "foo"], "1\n");

    assert!(output.status.success());
    assert!(stdout(&output).contains("Homepage: https://foo.example"));
}

#[test]
fn search_without_results_says_so() {
    let sandbox = Sandbox::new();

    let output = sandbox.command(&["search", "nope"], "");

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("No packages found for 'nope'."));
}

#[test]
fn search_failure_shows_the_real_error() {
    let sandbox = Sandbox::new();
    sandbox.tool(
        "nix",
        "echo \"error: unable to download: Could not resolve host\" >&2\nexit 1",
    );

    let output = sandbox.command(&["search", "foo"], "");

    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(error.contains("search error"), "{error}");
    assert!(error.contains("Could not resolve host"), "{error}");
    assert!(!stdout(&output).contains("No packages found"));
}

#[test]
fn reinstall_rebuilds_without_changing_the_file() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    let output = sandbox.command(&["reinstall", "git"], "");

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("does not necessarily download or compile"));
    assert_eq!(
        sandbox.commands(),
        "sudo nixos-rebuild switch --flake /etc/nixos#nixos\n"
    );
    assert_eq!(sandbox.content(), MODULE);
    assert!(!sandbox.backup().exists());
}

#[test]
fn reinstall_of_an_undeclared_package_suggests_install() {
    let sandbox = Sandbox::new();
    sandbox.write_config();

    let output = sandbox.command(&["reinstall", "vim"], "");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("nixblend install vim"));
    assert_eq!(sandbox.commands(), "");
}

#[test]
fn reinstall_stops_on_a_broken_configuration() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool("nix-instantiate", "echo 'error: broken' >&2\nexit 1");

    let output = sandbox.command(&["reinstall", "git"], "");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(sandbox.commands(), "");
}

#[test]
fn failed_reinstall_has_the_rebuild_exit_code() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool("sudo", "exit 7");

    let output = sandbox.command(&["reinstall", "git"], "");

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(sandbox.content(), MODULE);
}

#[test]
fn search_for_a_likely_typo_needs_consent() {
    let sandbox = Sandbox::new();
    sandbox.tool(
        "nix",
        &nix_answering(r#"["firefox"]"#, "{}", r#"{"never":"#),
    );

    let output = sandbox.command(&["search", "firefx"], "n\n");

    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("Did you mean: firefox?"), "{text}");
    assert!(text.contains("Search not run"), "{text}");
}

fn spawn_with_null_input(sandbox: &Sandbox, arguments: &[&str]) -> std::process::Child {
    let _lock = SPAWN_LOCK.lock().unwrap();
    Command::new(env!("CARGO_BIN_EXE_nixblend"))
        .args(arguments)
        .env("PATH", sandbox.bin())
        .env("XDG_DATA_HOME", sandbox.data_home())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
fn closed_pipe_ends_quietly_instead_of_panicking() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    // The tool waits, so that the reader is already gone when results are written.
    sandbox.tool(
        "nix",
        &format!(
            r#"{} 0.3; case "$*" in
  *attrNames*) echo '[]';;
  *" eval "*) echo '{{}}';;
  *) echo '{{"legacyPackages.x.foo":{{"pname":"foo","version":"1"}}}}';;
esac"#,
            which("sleep").display()
        ),
    );

    let mut child = spawn_with_null_input(&sandbox, &["search", "foo"]);
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();

    assert_eq!(output.status.code(), Some(141), "{}", stderr(&output));
    assert!(!stderr(&output).contains("panicked"), "{}", stderr(&output));
}

#[test]
fn being_terminated_ends_the_running_tool() {
    let sandbox = Sandbox::new();
    let marker = sandbox.root.path().join("tool.pid");
    sandbox.tool(
        "nix",
        &format!(
            r#"case "$*" in
  *attrNames*) echo '[]';;
  *) echo $$ > '{}'; {} 60 & wait;;
esac"#,
            marker.display(),
            which("sleep").display()
        ),
    );

    let mut child = spawn_with_null_input(&sandbox, &["search", "foo"]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while fs::read_to_string(&marker).map_or(true, |pid| pid.trim().is_empty()) {
        assert!(
            std::time::Instant::now() < deadline,
            "the tool did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let tool = fs::read_to_string(&marker).unwrap();
    let tool_dir = PathBuf::from("/proc").join(tool.trim());

    let pid = rustix::process::Pid::from_raw(child.id() as i32).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    child.wait().unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while tool_dir.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the tool is still running"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn a_killed_tool_gives_a_reason() {
    let sandbox = Sandbox::new();
    sandbox.tool("nix", "kill -9 $$");

    let output = sandbox.command(&["search", "foo"], "");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("run out of memory"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_killed_validator_is_not_an_invalid_expression() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool("nix-instantiate", "kill -9 $$");

    let output = sandbox.run("foo", "y\n");

    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(
        error.contains("could not run the Nix validation"),
        "{error}"
    );
    assert!(!error.contains("not valid Nix"), "{error}");
}

#[test]
fn a_term_that_looks_like_an_option_is_still_a_term() {
    let sandbox = Sandbox::new();
    let log = sandbox.log();
    sandbox.tool(
        "nix",
        &format!(
            r#"case "$*" in
  *attrNames*) echo '[]';;
  *" search "*) echo "$*" >> '{}'; echo '{{}}';;
  *) echo '{{}}';;
esac"#,
            log.display()
        ),
    );

    let output = sandbox.command(&["search", "--", "--version"], "");

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        sandbox.commands().ends_with(" -- --version\n"),
        "{}",
        sandbox.commands()
    );
}

#[test]
fn an_empty_search_term_is_refused() {
    let sandbox = Sandbox::new();
    let output = sandbox.command(&["search", ""], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("empty"));
}

#[test]
fn legitimate_words_search_without_a_question() {
    let sandbox = Sandbox::new();
    sandbox.tool(
        "nix",
        &nix_answering(
            r#"["browsers","cbrowser","p0f","pdd"]"#,
            "{}",
            r#"{"legacyPackages.x.w3m":{"pname":"w3m","version":"1","description":"A browser"}}"#,
        ),
    );

    for term in ["browser", "pdf"] {
        let output = sandbox.command(&["search", term], "");
        let text = stdout(&output);
        assert!(!text.contains("Did you mean"), "{text}");
        assert!(text.contains("Attribute: w3m"), "{text}");
    }
}

#[test]
fn likely_typo_searches_unless_told_not_to() {
    let sandbox = Sandbox::new();
    sandbox.tool(
        "nix",
        &nix_answering(
            r#"["firefox"]"#,
            "{}",
            r#"{"legacyPackages.x.firefox":{"pname":"firefox","version":"1"}}"#,
        ),
    );

    let output = sandbox.command(&["search", "firefx"], "");

    let text = stdout(&output);
    assert!(text.contains("Did you mean: firefox"), "{text}");
    assert!(text.contains("Attribute: firefox"), "{text}");
}

#[test]
fn nested_package_is_reachable_when_similar_names_are_offered() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool(
        "nix",
        &nix_answering(
            r#"["gate"]"#,
            r#"{"gate":{"pname":"gate","version":"1","description":"Proxy","available":true}}"#,
            r#"{"legacyPackages.x.kdePackages.kate":{"pname":"kate","version":"26","description":"Editor"}}"#,
        ),
    );

    let output = sandbox.run("kate", "s\ny\nn\n");

    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("1) gate"), "{text}");
    assert!(text.contains("s to search everything"), "{text}");
    assert!(sandbox.content().contains("    kdePackages.kate\n"));
}

#[test]
fn attribute_from_a_search_cannot_inject_code() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool(
        "nix",
        &nix_answering(
            "[]",
            "{}",
            r#"{"legacyPackages.x.evil ] ++ [ pkgs.foo":{"pname":"foo","version":"1"}}"#,
        ),
    );

    let output = sandbox.run("foo", "y\ny\nn\n");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("was not found"),
        "{}",
        stderr(&output)
    );
    assert_eq!(sandbox.content(), MODULE);
}

#[test]
fn failed_undo_still_reports_the_failed_rebuild() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.tool(
        "sudo",
        &format!(
            "echo '# changed' >> '{}'\nexit 7",
            sandbox.packages().display()
        ),
    );

    let output = sandbox.run("foo", "y\ny\ny\n");

    assert_eq!(output.status.code(), Some(3));
    let text = stdout(&output);
    assert!(text.contains("could not undo"), "{text}");
    assert!(stderr(&output).contains("rebuild error"));
    assert!(sandbox.content().contains("    foo\n"));
}

#[test]
fn control_characters_in_the_file_do_not_reach_the_terminal() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    sandbox.write_packages("{ pkgs }: [\n  \"\x1b[31mred\"\n  pkgs.git\n]\n");

    let output = sandbox.command(&["list"], "");

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!output.stdout.contains(&0x1b));
    assert!(stdout(&output).contains("[31mred"));
}

#[test]
fn info_explains_why_declaration_is_unknown() {
    let sandbox = Sandbox::new();
    fs::create_dir_all(sandbox.config().parent().unwrap()).unwrap();
    fs::write(sandbox.config(), "packages_file = [").unwrap();

    let output = sandbox.command(&["info", "foo"], "");

    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("Declared: Unknown (config error:"), "{text}");
}

#[test]
fn group_writable_config_is_not_trusted() {
    let sandbox = Sandbox::new();
    sandbox.write_config();
    fs::set_permissions(sandbox.config(), fs::Permissions::from_mode(0o660)).unwrap();

    let output = sandbox.command(&["list"], "");

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("writable by group"));
}
