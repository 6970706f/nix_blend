//! Command and argument definitions.

use clap::{Parser, Subcommand};

/// Package manager for NixOS.
#[derive(Debug, Parser)]
#[command(name = "nixblend", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Validates a package and declares it in the managed file
    Install {
        /// Package attribute, such as git, kdePackages.kate or unstable.vscode
        package: String,
        /// Do not offer to rebuild the system afterwards
        #[arg(long)]
        no_rebuild: bool,
    },
    /// Removes a package's declaration from the managed file
    Remove {
        /// Package attribute, such as git, pkgs.git or unstable.vscode
        package: String,
        /// Do not offer to rebuild the system afterwards
        #[arg(long)]
        no_rebuild: bool,
    },
    /// Searches package names and descriptions in nixpkgs
    Search {
        /// Text to search for
        term: String,
    },
    /// Lists the packages declared in the managed file
    List {
        /// Also compare with the active system generation
        #[arg(long)]
        active: bool,
    },
    /// Shows what is known about a package
    Info {
        /// Package attribute
        package: String,
    },
    /// Reapplies the configuration for a declared package. This runs a rebuild and does
    /// not necessarily download or compile the package again
    Reinstall {
        /// Package attribute
        package: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(arguments: &[&str]) -> Result<Command, clap::Error> {
        let mut all = vec!["nixblend"];
        all.extend_from_slice(arguments);
        Cli::try_parse_from(all).map(|cli| cli.command)
    }

    #[test]
    fn definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn install_requires_a_package() {
        assert!(parse(&["install"]).is_err());
        assert!(matches!(
            parse(&["install", "git"]).unwrap(),
            Command::Install { package, no_rebuild: false } if package == "git"
        ));
    }

    #[test]
    fn rebuild_can_be_skipped() {
        assert!(matches!(
            parse(&["remove", "git", "--no-rebuild"]).unwrap(),
            Command::Remove {
                no_rebuild: true,
                ..
            }
        ));
    }

    #[test]
    fn every_command_is_available() {
        assert!(matches!(
            parse(&["search", "web browser"]).unwrap(),
            Command::Search { term } if term == "web browser"
        ));
        assert!(matches!(
            parse(&["list", "--active"]).unwrap(),
            Command::List { active: true }
        ));
        assert!(matches!(
            parse(&["info", "git"]).unwrap(),
            Command::Info { .. }
        ));
        assert!(matches!(
            parse(&["reinstall", "git"]).unwrap(),
            Command::Reinstall { .. }
        ));
    }
}
