//! Reading, validation and persistence of the Nixblend configuration.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::atomic::{self, FileAttributes};
use super::user::{self, UserIdentity};
use crate::application::error::{AppError, ConfigError};
use crate::application::ports::{Config, ConfigStore};

const CONFIG_MODE: u32 = 0o600;
const DIRECTORY_MODE: u32 = 0o700;

/// Format of `config.toml`.
#[derive(Debug, Serialize, Deserialize)]
struct StoredConfig {
    packages_file: PathBuf,
}

/// Configuration of the user who started the command. All access happens with that
/// user's identity, even under `sudo`.
#[derive(Debug, Clone)]
pub struct UserConfigStore {
    pub user: UserIdentity,
    pub path: PathBuf,
}

impl ConfigStore for UserConfigStore {
    fn load(&self) -> Result<Option<Config>, AppError> {
        let _guard = user::drop_privileges(&self.user)?;
        Ok(load(&self.path, self.user.uid)?)
    }

    fn save(&self, config: &Config) -> Result<(), AppError> {
        let _guard = user::drop_privileges(&self.user)?;
        Ok(save(&self.path, config)?)
    }

    fn parse_path(&self, input: &str) -> Result<PathBuf, ConfigError> {
        parse_packages_path(input, &self.user.home)
    }

    fn location(&self) -> PathBuf {
        self.path.clone()
    }
}

/// Path of `config.toml` for the user with the given home directory.
///
/// A relative `XDG_DATA_HOME` is ignored, as the XDG specification requires. When the
/// process is elevated, the value is only accepted inside the user's home, because the
/// environment cannot be trusted to choose where data is written.
pub fn file_path(home: &Path, xdg_data_home: Option<&OsStr>, elevated: bool) -> PathBuf {
    xdg_data_home
        .map(Path::new)
        .filter(|path| {
            let safe =
                path.is_absolute() && !path.components().any(|part| part == Component::ParentDir);
            safe && (!elevated || path.starts_with(home))
        })
        .map_or_else(|| home.join(".local/share"), Path::to_path_buf)
        .join("nixblend/config.toml")
}

/// Loads the configuration. Returns `None` if it does not exist yet. A file another user
/// could have written is not trusted to say which file to manage.
fn load(path: &Path, owner: u32) -> Result<Option<Config>, ConfigError> {
    let read_error = |source| ConfigError::Read {
        path: path.to_owned(),
        source,
    };
    let invalid = |message: String| ConfigError::Invalid {
        path: path.to_owned(),
        message,
    };

    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(read_error(source)),
    };
    if metadata.uid() != owner || metadata.mode() & 0o002 != 0 {
        return Err(invalid(format!(
            "it must belong to user {owner} and not be writable by others"
        )));
    }

    let content = fs::read_to_string(path).map_err(read_error)?;
    let stored: StoredConfig =
        toml::from_str(&content).map_err(|error| invalid(error.message().to_owned()))?;
    // The file can be edited by hand, so it is as untrusted as a typed answer.
    check_packages_path(&stored.packages_file).map_err(|error| invalid(error.to_string()))?;
    Ok(Some(Config {
        packages_file: stored.packages_file,
    }))
}

/// Writes the configuration atomically, creating the required directories.
fn save(path: &Path, config: &Config) -> Result<(), ConfigError> {
    let stored = StoredConfig {
        packages_file: config.packages_file.clone(),
    };
    let content =
        toml::to_string(&stored).map_err(|error| ConfigError::Serialize(error.to_string()))?;
    let write_error = |source| ConfigError::Write {
        path: path.to_owned(),
        source,
    };

    if let Some(parent) = path.parent() {
        DirBuilder::new()
            .recursive(true)
            .mode(DIRECTORY_MODE)
            .create(parent)
            .map_err(write_error)?;
    }
    let attributes = FileAttributes {
        mode: CONFIG_MODE,
        owner: None,
    };
    atomic::write_file(path, content.as_bytes(), attributes).map_err(write_error)
}

fn check_packages_path(path: &Path) -> Result<(), ConfigError> {
    let text = || path.display().to_string();
    if !path.is_absolute() {
        return Err(ConfigError::RelativePath(text()));
    }
    if path.extension() != Some(OsStr::new("nix")) {
        return Err(ConfigError::NotNixFile(text()));
    }
    Ok(())
}

/// Validates the form of the `packages.nix` path typed by the user, expanding `~/` to
/// the home directory of the original user. The file itself is checked when it is
/// opened, with the access rules of an edit.
fn parse_packages_path(input: &str, home: &Path) -> Result<PathBuf, ConfigError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(ConfigError::MissingPath);
    }
    let path = match input.strip_prefix("~/") {
        Some(relative) => home.join(relative),
        None => PathBuf::from(input),
    };
    check_packages_path(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const HOME: &str = "/home/alice";

    fn me() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    fn config_path(xdg: Option<&str>, elevated: bool) -> PathBuf {
        file_path(Path::new(HOME), xdg.map(OsStr::new), elevated)
    }

    fn config(path: &str) -> Config {
        Config {
            packages_file: PathBuf::from(path),
        }
    }

    #[test]
    fn falls_back_to_local_share() {
        assert_eq!(
            config_path(None, false),
            Path::new("/home/alice/.local/share/nixblend/config.toml")
        );
    }

    #[test]
    fn respects_xdg_data_home() {
        assert_eq!(
            config_path(Some("/data/alice"), false),
            Path::new("/data/alice/nixblend/config.toml")
        );
    }

    #[test]
    fn ignores_relative_xdg_data_home() {
        assert_eq!(config_path(Some("data"), false), config_path(None, false));
    }

    #[test]
    fn elevated_process_only_accepts_xdg_inside_home() {
        assert_eq!(
            config_path(Some("/home/alice/data"), true),
            Path::new("/home/alice/data/nixblend/config.toml")
        );
        assert_eq!(config_path(Some("/etc"), true), config_path(None, true));
        assert_eq!(
            config_path(Some("/home/alice/../../etc"), true),
            config_path(None, true)
        );
    }

    #[test]
    fn load_returns_none_when_missing() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            load(&directory.path().join("config.toml"), me()).unwrap(),
            None
        );
    }

    #[test]
    fn save_and_load_round_trip_with_private_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a/b/config.toml");
        let expected = config("/etc/nixos/\"pack\" ages.nix");

        save(&path, &expected).unwrap();

        assert_eq!(load(&path, me()).unwrap(), Some(expected));
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, CONFIG_MODE);
        let parent = fs::metadata(path.parent().unwrap()).unwrap();
        assert_eq!(parent.mode() & 0o777, DIRECTORY_MODE);
    }

    #[test]
    fn invalid_config_is_reported_and_kept() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "packages_file = [").unwrap();

        assert!(matches!(
            load(&path, me()),
            Err(ConfigError::Invalid { .. })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "packages_file = [");
    }

    #[test]
    fn config_without_packages_file_is_invalid() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "other = 1\n").unwrap();

        assert!(matches!(
            load(&path, me()),
            Err(ConfigError::Invalid { .. })
        ));
    }

    #[test]
    fn load_rejects_paths_a_prompt_would_reject() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        for value in ["rel.nix", "/etc/shadow", "/etc/nixos/notes.txt"] {
            fs::write(&path, format!("packages_file = \"{value}\"\n")).unwrap();
            assert!(
                matches!(load(&path, me()), Err(ConfigError::Invalid { .. })),
                "{value}"
            );
        }
    }

    #[test]
    fn load_rejects_files_others_could_have_written() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "packages_file = \"/etc/nixos/p.nix\"\n").unwrap();

        assert!(load(&path, me()).is_ok());
        assert!(matches!(
            load(&path, me() + 1),
            Err(ConfigError::Invalid { .. })
        ));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(matches!(
            load(&path, me()),
            Err(ConfigError::Invalid { .. })
        ));
    }

    #[test]
    fn save_reports_unwritable_location() {
        let directory = tempfile::tempdir().unwrap();
        let blocker = directory.path().join("file");
        fs::write(&blocker, "").unwrap();

        let error = save(&blocker.join("config.toml"), &config("/x.nix")).unwrap_err();

        assert!(matches!(error, ConfigError::Write { .. }));
    }

    #[test]
    fn packages_path_accepts_nix_files_and_expands_home() {
        let home = Path::new(HOME);
        assert_eq!(
            parse_packages_path(" /etc/nixos/packages.nix \n", home).unwrap(),
            Path::new("/etc/nixos/packages.nix")
        );
        assert_eq!(
            parse_packages_path("~/packages.nix", home).unwrap(),
            Path::new("/home/alice/packages.nix")
        );
    }

    #[test]
    fn packages_path_rejects_invalid_input() {
        let home = Path::new(HOME);
        assert!(matches!(
            parse_packages_path("  ", home),
            Err(ConfigError::MissingPath)
        ));
        assert!(matches!(
            parse_packages_path("packages.nix", home),
            Err(ConfigError::RelativePath(_))
        ));
        assert!(matches!(
            parse_packages_path("/etc/nixos/packages.txt", home),
            Err(ConfigError::NotNixFile(_))
        ));
    }
}
