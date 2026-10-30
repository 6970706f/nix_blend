//! Validation of Nix expressions with the real parser.

use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::process::{self, ProcessError};
use super::tools;
use super::user::UserIdentity;
use crate::application::error::EditError;
use crate::application::ports::SyntaxCheck;

const NIX_INSTANTIATE: &str = "nix-instantiate";
const TIMEOUT: Duration = Duration::from_secs(60);

/// Checks the syntax of `content` with `nix-instantiate --parse`, without evaluating
/// it. The parser also reports undefined variables. The expression comes from an open
/// file, so the tool needs no access to its path and can run without privileges.
pub fn check_syntax(
    content: File,
    elevated: bool,
    run_as: Option<&UserIdentity>,
) -> Result<SyntaxCheck, EditError> {
    let program = tools::program(NIX_INSTANTIATE, elevated).map_err(EditError::ValidationFailed)?;
    let mut command = Command::new(program);
    tools::apply_environment(&mut command, elevated, run_as);
    check_syntax_with(command, content)
}

fn check_syntax_with(mut command: Command, content: File) -> Result<SyntaxCheck, EditError> {
    command
        .args(["--parse", "-"])
        .stdin(Stdio::from(content))
        // Never the caller's directory, which the tool may not be allowed to read.
        .current_dir(Path::new("/"));
    match process::run(&mut command, TIMEOUT, &mut |_| {}) {
        Ok(output) if output.status.success() => Ok(SyntaxCheck::Valid),
        Ok(output) => Err(EditError::InvalidNix(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        )),
        Err(ProcessError::NotFound) => Ok(SyntaxCheck::ToolUnavailable),
        Err(ProcessError::Spawn(error)) => Err(EditError::ValidationFailed(error.to_string())),
        Err(ProcessError::TimedOut(limit)) => Err(EditError::ValidationFailed(format!(
            "no answer within {limit:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, Write};

    fn file_with(content: &str) -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.rewind().unwrap();
        file
    }

    fn check(program: &str) -> Result<SyntaxCheck, EditError> {
        check_syntax_with(Command::new(program), file_with(""))
    }

    #[test]
    fn successful_exit_means_valid() {
        assert_eq!(check("true").unwrap(), SyntaxCheck::Valid);
    }

    #[test]
    fn failed_exit_is_a_validation_error() {
        assert!(matches!(check("false"), Err(EditError::InvalidNix(_))));
    }

    #[test]
    fn missing_tool_is_reported_as_unavailable() {
        let result = check("/nonexistent/nix-instantiate");
        assert_eq!(result.unwrap(), SyntaxCheck::ToolUnavailable);
    }

    #[test]
    fn real_tool_accepts_valid_and_rejects_invalid_syntax() {
        let valid = check_syntax(file_with("{ pkgs }: with pkgs; [ git ]"), false, None).unwrap();
        if valid == SyntaxCheck::ToolUnavailable {
            eprintln!("SKIPPED real syntax check: nix-instantiate unavailable");
            return;
        }
        for rejected in ["{ pkgs }: with pkgs; [ git ", "with pkgs; [ git ]"] {
            assert!(
                matches!(
                    check_syntax(file_with(rejected), false, None),
                    Err(EditError::InvalidNix(_))
                ),
                "{rejected}"
            );
        }
    }
}
