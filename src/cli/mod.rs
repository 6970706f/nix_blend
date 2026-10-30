//! Command line: receives arguments, calls use cases and presents their results.

mod args;
mod presenter;
pub mod progress;
pub mod terminal;

pub use args::{Cli, Command};

use std::process::ExitCode;

use crate::application::error::{AppError, ConfigError};
use crate::application::get_package_info::GetPackageInfo;
use crate::application::install_package::InstallPackage;
use crate::application::list_packages::ListPackages;
use crate::application::ports::{Choice, Services};
use crate::application::reinstall_package::ReinstallPackage;
use crate::application::remove_package::RemovePackage;
use crate::application::search_packages::SearchPackages;

/// Options shown at once in search results.
const SEARCH_PAGE: usize = 20;

/// Runs a command with the given services and returns the process exit code.
pub fn run(command: &Command, services: Services) -> ExitCode {
    match dispatch(command, services) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            exit_code(&error)
        }
    }
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
            print!("{}", presenter::package_list(&list));
            Ok(())
        }
        Command::Info { package } => {
            let info = GetPackageInfo { services }.execute(package)?;
            print!("{}", presenter::package_info(&info));
            Ok(())
        }
        Command::Search { term } => search(term, services),
    }
}

/// Shows results a page at a time, and the details of the ones the user selects.
fn search(term: &str, services: Services) -> Result<(), AppError> {
    let use_case = SearchPackages { services };
    let Some(results) = use_case.execute(term)? else {
        println!("Search not run. Try one of the names above.");
        return Ok(());
    };
    if results.packages.is_empty() {
        println!("No packages found for '{term}'.");
        return Ok(());
    }
    if !results.pinned {
        eprintln!(
            "Warning: could not read the nixpkgs revision from the system flake.lock; using the flake registry."
        );
    }

    let mut shown = 0;
    loop {
        let end = results.packages.len().min(shown + SEARCH_PAGE);
        let page = use_case.with_details(&results.packages[shown..end]);
        for package in &page {
            println!("{}", presenter::search_result(package));
        }
        shown = end;
        let has_more = shown < results.packages.len();
        println!("Showing {shown} of {} result(s).", results.packages.len());

        let choice = services.ui.choose_package(term, &[], shown, has_more)?;
        match choice {
            Choice::More => {}
            Choice::Cancel => return Ok(()),
            Choice::Index(index) => {
                let attr = &results.packages[index].identity.attr;
                match (GetPackageInfo { services }).execute(attr) {
                    Ok(info) => print!("{}", presenter::package_info(&info)),
                    Err(AppError::Config(ConfigError::NotConfigured)) => {}
                    Err(error) => eprintln!("{error}"),
                }
                return Ok(());
            }
        }
    }
}
