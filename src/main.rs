//! Nixblend: a package manager for NixOS.
//!
//! The dependencies point inwards: `cli` and `tui` call `application`, which uses `domain`,
//! and `infrastructure` implements the interfaces `application` declares. This file is the
//! only place that knows all of them, because it connects them.

mod application;
mod cli;
mod domain;
mod infrastructure;
mod tui;

use std::env;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;

use application::error::AppError;
use application::ports::{Interaction, ProgressReporter, Services, SystemRebuilder};
use cli::Cli;
use cli::progress::TerminalProgress;
use cli::terminal::Terminal;
use infrastructure::config::{self, UserConfigStore};
use infrastructure::generation::CurrentSystem;
use infrastructure::managed_file::NixFileStore;
use infrastructure::nix_catalog::NixCatalogs;
use infrastructure::nix_file::NixPackageDocument;
use infrastructure::rebuild::NixosRebuild;
use infrastructure::user::UserIdentity;
use infrastructure::{atomic, process, signals, user};

const HOSTNAME_PATH: &str = "/proc/sys/kernel/hostname";
const NIXOS_MARKER: &str = "/etc/NIXOS";
/// Exit code of a program that was asked to stop with Ctrl+C.
const INTERRUPTED: u8 = 130;

/// Connects the adapters and hands the result to `body`.
fn with_services(
    identity: &UserIdentity,
    is_root: bool,
    ui: &dyn Interaction,
    progress: &dyn ProgressReporter,
    rebuilder: &dyn SystemRebuilder,
    body: &mut dyn FnMut(Services),
) {
    let xdg_data_home = env::var_os("XDG_DATA_HOME");
    let config = UserConfigStore {
        path: config::file_path(&identity.home, xdg_data_home.as_deref(), is_root),
        user: identity.clone(),
    };
    let files = NixFileStore {
        user: identity.clone(),
        is_root,
    };
    let catalogs = NixCatalogs {
        user: identity.clone(),
        elevated: is_root,
        progress,
    };
    body(Services {
        config: &config,
        files: &files,
        document: &NixPackageDocument,
        catalogs: &catalogs,
        rebuilder,
        generation: &CurrentSystem { elevated: is_root },
        ui,
    });
}

fn system_label() -> String {
    let host = fs::read_to_string(HOSTNAME_PATH).unwrap_or_default();
    let host = host.trim();
    let system = Path::new(NIXOS_MARKER).exists().then_some("NixOS");
    match (system, host.is_empty()) {
        (Some(system), false) => format!("{system} / {host}"),
        (Some(system), true) => system.to_owned(),
        (None, _) => host.to_owned(),
    }
}

fn run_tui(identity: UserIdentity, is_root: bool) -> ExitCode {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        cli::report_no_terminal();
        return ExitCode::from(2);
    }
    signals::on_exit(tui::restore_terminal);
    let backend = tui::Backend {
        context: system_label(),
        rebuilder: Box::new(NixosRebuild { is_root }),
        compose: Box::new(move |ui, progress, rebuilder, body| {
            with_services(&identity, is_root, ui, progress, rebuilder, body);
        }),
        cancel: process::terminate_all,
        cleanup: || {
            process::terminate_all();
            atomic::remove_registered();
        },
    };
    match tui::run(backend) {
        Ok(tui::Outcome::Quit) => ExitCode::SUCCESS,
        Ok(tui::Outcome::Interrupted) => ExitCode::from(INTERRUPTED),
        Err(error) => {
            cli::report(&format!("could not run the interactive interface: {error}"));
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    signals::install_cleanup();

    let is_root = user::is_root();
    let identity = match user::current() {
        Ok(identity) => identity,
        Err(error) => {
            cli::report(&AppError::from(error).to_string());
            return ExitCode::FAILURE;
        }
    };

    let Some(command) = cli.command else {
        return run_tui(identity, is_root);
    };
    let progress = TerminalProgress::new();
    let terminal = Terminal::new(io::stdin().lock(), io::stdout());
    let mut exit_code = ExitCode::FAILURE;
    with_services(
        &identity,
        is_root,
        &terminal,
        &progress,
        &NixosRebuild { is_root },
        &mut |services| exit_code = cli::run(&command, services),
    );
    exit_code
}
