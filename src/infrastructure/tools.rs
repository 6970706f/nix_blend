//! Execution environment of external tools.
//!
//! While elevated, the caller's environment must not decide what runs as root, so tools
//! come from the current system profile, are checked before use, and only inherit
//! variables known to be harmless. Too many variables change how some tool loads code
//! or reads its settings for a list of the dangerous ones to be complete.

use std::env;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::user::UserIdentity;

const TRUSTED_BIN: &str = "/run/current-system/sw/bin";
const TRUSTED_PATH: &str = "/run/wrappers/bin:/run/current-system/sw/bin";
const ROOT_HOME: &str = "/root";
const INHERITED_VARIABLES: [&str; 18] = [
    "COLORTERM",
    "LANG",
    "LANGUAGE",
    "LOCALE_ARCHIVE",
    "NO_COLOR",
    "SSH_AUTH_SOCK",
    "SUDO_GID",
    "SUDO_UID",
    "SUDO_USER",
    "TERM",
    "TERMINFO_DIRS",
    "TZ",
    "all_proxy",
    "ftp_proxy",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "rsync_proxy",
];
const INHERITED_PREFIX: &str = "LC_";

/// Program to execute for `name`: resolved through `PATH` normally. When elevated, it
/// comes from the system profile and must be owned by root and not writable by others.
/// A missing tool is not an error here, and is reported when it fails to start.
pub fn program(name: &str, elevated: bool) -> Result<PathBuf, String> {
    if !elevated {
        return Ok(PathBuf::from(name));
    }
    let path = Path::new(TRUSTED_BIN).join(name);
    match fs::metadata(&path) {
        Ok(metadata) if is_trusted(&metadata) => Ok(path),
        Ok(_) => Err(format!(
            "refusing to run {}: unsafe owner or mode",
            path.display()
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(path),
        Err(error) => Err(format!("could not check {}: {error}", path.display())),
    }
}

fn is_trusted(metadata: &fs::Metadata) -> bool {
    metadata.uid() == 0 && metadata.mode() & 0o022 == 0
}

/// Applies the environment rules to a command that will run a tool. With `run_as`, the
/// tool runs as that user and uses that user's home, so its caches never become owned
/// by root.
pub fn apply_environment(command: &mut Command, elevated: bool, run_as: Option<&UserIdentity>) {
    if elevated {
        command.env_clear();
        for (name, value) in env::vars_os() {
            let inherited = name.to_str().is_some_and(|name| {
                INHERITED_VARIABLES.contains(&name) || name.starts_with(INHERITED_PREFIX)
            });
            if inherited {
                command.env(name, value);
            }
        }
        command
            .env("PATH", TRUSTED_PATH)
            .env("HOME", ROOT_HOME)
            .env("USER", "root")
            .env("LOGNAME", "root");
    }
    if let Some(user) = run_as {
        command
            .uid(user.uid)
            .gid(user.gid)
            .env("HOME", &user.home)
            .env("USER", &user.name)
            .env("LOGNAME", &user.name)
            .env_remove("XDG_CACHE_HOME");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::fs::PermissionsExt;

    fn path_of(command: &Command) -> Option<&OsStr> {
        command
            .get_envs()
            .find(|(name, _)| *name == "PATH")
            .and_then(|(_, value)| value)
    }

    #[test]
    fn unprivileged_tools_use_the_callers_path() {
        assert_eq!(program("nix", false).unwrap(), Path::new("nix"));
        let mut command = Command::new("nix");
        apply_environment(&mut command, false, None);
        assert_eq!(path_of(&command), None);
    }

    #[test]
    fn elevated_tools_ignore_the_callers_path() {
        let mut command = Command::new("nix");
        apply_environment(&mut command, true, None);
        assert_eq!(path_of(&command), Some(OsStr::new(TRUSTED_PATH)));
    }

    #[test]
    fn missing_elevated_tool_is_left_to_fail_when_started() {
        assert_eq!(
            program("no-such-tool", true).unwrap(),
            Path::new("/run/current-system/sw/bin/no-such-tool")
        );
    }

    #[test]
    fn only_root_owned_files_are_trusted() {
        // A file created by a test is not owned by root, unless the test runs as root.
        if rustix::process::geteuid().is_root() {
            eprintln!("SKIPPED trust check: running as root");
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("tool");
        fs::write(&file, "").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!is_trusted(&fs::metadata(&file).unwrap()));
    }
}
