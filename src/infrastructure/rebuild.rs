//! NixOS rebuild execution.

use std::io;
use std::process::{Command, ExitStatus};

use super::tools;
use crate::application::error::RebuildError;
use crate::application::ports::SystemRebuilder;

const NIXOS_REBUILD: &str = "nixos-rebuild";
const SUDO: &str = "sudo";
const ARGUMENTS: [&str; 3] = ["switch", "--flake", "/etc/nixos#nixos"];

/// Rebuilds with `nixos-rebuild`, with the inherited terminal, leaving the output and
/// any `sudo` password prompt to the tools themselves. It has no time limit: a rebuild
/// can legitimately take hours, and the user sees its progress.
#[derive(Debug, Clone, Copy)]
pub struct NixosRebuild {
    pub is_root: bool,
}

impl SystemRebuilder for NixosRebuild {
    fn rebuild(&self) -> Result<(), RebuildError> {
        let mut command = build_command(self.is_root)?;
        let status = command.status();
        interpret(command.get_program().to_string_lossy().as_ref(), status)
    }
}

/// The rebuild requires root: otherwise, elevation is delegated to `sudo`. A process
/// that is not root runs with the caller's own authority, so its `PATH` is no threat.
fn build_command(is_root: bool) -> Result<Command, RebuildError> {
    let mut command = if is_root {
        let program = tools::program(NIXOS_REBUILD, true).map_err(RebuildError::UntrustedTool)?;
        Command::new(program)
    } else {
        let mut sudo = Command::new(SUDO);
        sudo.arg(NIXOS_REBUILD);
        sudo
    };
    tools::apply_environment(&mut command, is_root, None);
    command.args(ARGUMENTS);
    Ok(command)
}

fn interpret(program: &str, status: io::Result<ExitStatus>) -> Result<(), RebuildError> {
    let status = status.map_err(|source| RebuildError::Spawn {
        program: program.to_owned(),
        source,
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(RebuildError::Failed(status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn parts(command: &Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|part| part.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn root_runs_nixos_rebuild_from_the_system_profile() {
        let Ok(command) = build_command(true) else {
            eprintln!("SKIPPED: the system profile cannot be trusted in this environment");
            return;
        };
        assert_eq!(
            parts(&command),
            [
                "/run/current-system/sw/bin/nixos-rebuild",
                "switch",
                "--flake",
                "/etc/nixos#nixos"
            ]
        );
    }

    #[test]
    fn regular_user_elevates_through_sudo() {
        assert_eq!(
            parts(&build_command(false).unwrap()),
            [
                "sudo",
                "nixos-rebuild",
                "switch",
                "--flake",
                "/etc/nixos#nixos"
            ]
        );
    }

    #[test]
    fn exit_status_is_checked() {
        assert!(interpret("x", Ok(ExitStatus::from_raw(0))).is_ok());
        assert!(matches!(
            interpret("x", Ok(ExitStatus::from_raw(1 << 8))),
            Err(RebuildError::Failed(_))
        ));
    }

    #[test]
    fn spawn_failure_names_the_program() {
        let error = interpret("sudo", Err(io::ErrorKind::NotFound.into())).unwrap_err();
        assert!(matches!(error, RebuildError::Spawn { .. }));
        assert!(error.to_string().contains("sudo"));
    }
}
