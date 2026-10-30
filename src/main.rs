//! Nixblend: a package manager for NixOS.
//!
//! The dependencies point inwards: `cli` calls `application`, which uses `domain`, and
//! `infrastructure` implements the interfaces `application` declares. This file is the
//! only place that knows all of them, because it connects them.

mod application;
mod cli;
mod domain;
mod infrastructure;

use std::env;
use std::io;
use std::process::ExitCode;

use clap::Parser;

use application::error::AppError;
use application::ports::Services;
use cli::Cli;
use cli::progress::TerminalProgress;
use cli::terminal::Terminal;
use infrastructure::config::{self, UserConfigStore};
use infrastructure::generation::CurrentSystem;
use infrastructure::managed_file::NixFileStore;
use infrastructure::nix_catalog::NixCatalogs;
use infrastructure::nix_file::NixPackageDocument;
use infrastructure::rebuild::NixosRebuild;
use infrastructure::{signals, user};

fn main() -> ExitCode {
    let cli = Cli::parse();
    signals::install_cleanup();

    let is_root = user::is_root();
    let identity = match user::current() {
        Ok(identity) => identity,
        Err(error) => {
            eprintln!("{}", AppError::from(error));
            return ExitCode::FAILURE;
        }
    };
    let xdg_data_home = env::var_os("XDG_DATA_HOME");

    let config = UserConfigStore {
        path: config::file_path(&identity.home, xdg_data_home.as_deref(), is_root),
        user: identity.clone(),
    };
    let files = NixFileStore {
        user: identity.clone(),
        is_root,
    };
    let progress = TerminalProgress::new();
    let catalogs = NixCatalogs {
        user: identity,
        elevated: is_root,
        progress: &progress,
    };
    let terminal = Terminal::new(io::stdin().lock(), io::stdout());

    let services = Services {
        config: &config,
        files: &files,
        document: &NixPackageDocument,
        catalogs: &catalogs,
        rebuilder: &NixosRebuild { is_root },
        generation: &CurrentSystem { elevated: is_root },
        ui: &terminal,
    };
    cli::run(&cli.command, services)
}
