//! Command line: receives arguments, calls use cases and presents their results.

mod args;
pub mod presenter;
pub mod progress;
pub mod terminal;

pub use args::{Cli, Command};

use std::io::{self, Write};
use std::process::ExitCode;

use crate::application::error::AppError;
use crate::application::get_package_info::GetPackageInfo;
use crate::application::install_package::InstallPackage;
use crate::application::list_packages::ListPackages;
use crate::application::ports::{Choice, Services};
use crate::application::reinstall_package::ReinstallPackage;
use crate::application::remove_package::RemovePackage;
use crate::application::search_packages::SearchPackages;

/// Options shown at once in search results.
const SEARCH_PAGE: usize = 20;
/// Exit code of a program ended by a closed pipe, as a shell would report it.
const PIPE_CLOSED: u8 = 141;

/// Runs a command with the given services and returns the process exit code.
pub fn run(command: &Command, services: Services) -> ExitCode {
    match dispatch(command, services) {
        Ok(()) => ExitCode::SUCCESS,
        // Whoever was reading is gone, as in `nixblend search foo | head`, so this is the
        // conventional end of such a program, with nobody to tell.
        Err(AppError::Terminal(error)) if error.kind() == io::ErrorKind::BrokenPipe => {
            ExitCode::from(PIPE_CLOSED)
        }
        Err(error) => {
            report(&error.to_string());
            exit_code(&error)
        }
    }
}

/// Writes to the standard error. If it is closed, there is nowhere left to complain to.
pub fn report(message: &str) {
    let _ = writeln!(io::stderr(), "{}", presenter::sanitize(message));
}

fn show(text: &str) -> io::Result<()> {
    let mut stdout = io::stdout();
    stdout.write_all(presenter::sanitize(text).as_bytes())?;
    stdout.flush()
}

/// The rebuild has its own code so that scripts can tell an applied edit from an edit
/// that failed.
fn exit_code(error: &AppError) -> ExitCode {
    match error {
        AppError::Rebuild(_) => ExitCode::from(3),
        _ => ExitCode::FAILURE,
    }
}

fn dispatch(command: &Command, services: Services) -> Result<(), AppError> {
    match command {
        Command::Install {
            package,
            no_rebuild,
        } => InstallPackage { services }.execute(package, *no_rebuild),
        Command::Remove {
            package,
            no_rebuild,
        } => RemovePackage { services }.execute(package, *no_rebuild),
        Command::Reinstall { package } => ReinstallPackage { services }.execute(package),
        Command::List { active } => {
            let list = ListPackages { services }.execute(*active)?;
            show(&presenter::package_list(&list))?;
            Ok(())
        }
        Command::Info { package } => {
            let info = GetPackageInfo { services }.execute(package)?;
            show(&presenter::package_info(&info))?;
            Ok(())
        }
        Command::Search { term } => search(term, services),
    }
}

/// Shows results a page at a time, and the details of the ones the user selects.
fn search(term: &str, services: Services) -> Result<(), AppError> {
    let use_case = SearchPackages { services };
    let Some(results) = use_case.execute(term)? else {
        show("Search not run. Try one of the names above.\n")?;
        return Ok(());
    };
    if results.packages.is_empty() {
        show(&format!("No packages found for '{term}'.\n"))?;
        return Ok(());
    }
    if !results.pinned {
        report(
            "Warning: could not read the nixpkgs revision from the system flake.lock; using the flake registry.",
        );
    }

    let mut shown = 0;
    loop {
        let end = results.packages.len().min(shown + SEARCH_PAGE);
        let (page, warning) = use_case.with_details(&results.packages[shown..end]);
        if let Some(warning) = warning {
            report(&format!("Warning: {warning}"));
        }
        for package in &page {
            show(&format!("{}\n", presenter::search_result(package)))?;
        }
        shown = end;
        let has_more = shown < results.packages.len();
        show(&format!(
            "Showing {shown} of {} result(s).\n",
            results.packages.len()
        ))?;

        let choice = services
            .ui
            .choose_package(term, &[], shown, has_more, false)?;
        match choice {
            Choice::More => {}
            Choice::Cancel | Choice::SearchAll => return Ok(()),
            Choice::Index(index) => {
                let attr = &results.packages[index].identity.attr;
                let info = GetPackageInfo { services }.execute(attr)?;
                return show(&presenter::package_info(&info)).map_err(AppError::from);
            }
        }
    }
}

/// Explains why the interactive interface cannot open: there is no terminal to draw on.
pub fn report_no_terminal() {
    report("The interactive interface needs a terminal. Use a command instead, such as:");
    report("  nixblend search <term>");
    report("  nixblend list");
    report("See 'nixblend --help' for all of them.");
}
