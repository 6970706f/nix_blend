//! The active system generation.

use std::collections::HashSet;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::process;
use super::tools;
use crate::application::error::AppError;
use crate::application::ports::ActiveGeneration;

const NIX_STORE: &str = "nix-store";
const CURRENT_SYSTEM: &str = "/run/current-system";
const TIMEOUT: Duration = Duration::from_secs(60);

/// Reads the closure of `/run/current-system`.
#[derive(Debug, Clone, Copy)]
pub struct CurrentSystem {
    pub elevated: bool,
}

impl ActiveGeneration for CurrentSystem {
    fn store_names(&self) -> Result<HashSet<String>, AppError> {
        let program = tools::program(NIX_STORE, self.elevated).map_err(AppError::Generation)?;
        let mut command = Command::new(program);
        tools::apply_environment(&mut command, self.elevated, None);
        command
            .args(["--query", "--requisites", CURRENT_SYSTEM])
            .stdin(Stdio::null());
        let output = process::run(&mut command, TIMEOUT, &mut |_| {})
            .map_err(|error| AppError::Generation(format!("{error:?}")))?;
        if !output.status.success() {
            return Err(AppError::Generation(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(parse_store_names(&String::from_utf8_lossy(&output.stdout)))
    }
}

/// Reduces `/nix/store/<hash>-<name>` lines to `<name>`.
fn parse_store_names(output: &str) -> HashSet<String> {
    output
        .lines()
        .filter_map(|line| line.rsplit('/').next()?.split_once('-'))
        .map(|(_hash, name)| name.to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_paths_are_reduced_to_names() {
        let names = parse_store_names(
            "/nix/store/abc123-firefox-158.0\n/nix/store/def456-yt-dlp-2026\nnot a path\n",
        );
        assert!(names.contains("firefox-158.0"));
        assert!(names.contains("yt-dlp-2026"));
        assert_eq!(names.len(), 2);
    }
}
