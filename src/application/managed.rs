//! Access to the managed file, shared by the use cases.

use super::error::{AppError, ConfigError, EditError};
use super::ports::{Config, FileMode, ManagedFile, Notice, Services, SyntaxCheck};
use crate::domain::package::DeclaredPackages;

/// The managed file and what it declares.
pub struct Managed {
    pub file: Box<dyn ManagedFile>,
    pub declared: DeclaredPackages,
}

/// Opens the managed file for editing, asking for its path on the first run. A new
/// path is saved only after it proved usable, so that a wrong answer does not stick.
pub fn open_for_edit(services: Services) -> Result<Managed, AppError> {
    let loaded = services.config.load()?;
    let is_new = loaded.is_none();
    let config = match loaded {
        Some(config) => config,
        None => ask_for_config(services)?,
    };

    match open(services, &config, FileMode::ReadWrite) {
        Ok(managed) => {
            if is_new {
                services.config.save(&config)?;
            }
            Ok(managed)
        }
        Err(error) => {
            services.ui.notify(&if is_new {
                Notice::PathNotSaved
            } else {
                Notice::ManagedFileConfiguredIn(services.config.location())
            })?;
            Err(error)
        }
    }
}

/// Opens the managed file for reading. Without a configuration, there is nothing to
/// read and no reason to ask for a path.
pub fn open_for_reading(services: Services) -> Result<Managed, AppError> {
    let config = services.config.load()?.ok_or(ConfigError::NotConfigured)?;
    open(services, &config, FileMode::ReadOnly).inspect_err(|_| {
        let notice = Notice::ManagedFileConfiguredIn(services.config.location());
        let _ = services.ui.notify(&notice);
    })
}

fn open(services: Services, config: &Config, mode: FileMode) -> Result<Managed, AppError> {
    let file = services.files.open(&config.packages_file, mode)?;
    let declared = services.document.analyze(file.content())?;
    Ok(Managed { file, declared })
}

fn ask_for_config(services: Services) -> Result<Config, AppError> {
    let answer = services
        .ui
        .ask_packages_path()?
        .ok_or(ConfigError::MissingPath)?;
    Ok(Config {
        packages_file: services.config.parse_path(&answer)?,
    })
}

/// Validates the file as it is now. A file that is already broken must not be
/// edited: the failure would be blamed on the edit, or hidden by it.
pub fn check_current(services: Services, file: &dyn ManagedFile) -> Result<(), AppError> {
    if file.check_current().map_err(current_failure)? == SyntaxCheck::ToolUnavailable {
        services.ui.notify(&Notice::SyntaxNotChecked)?;
    }
    Ok(())
}

fn current_failure(error: AppError) -> AppError {
    match error {
        AppError::Edit(EditError::InvalidNix(message)) => {
            EditError::InvalidCurrentNix(message).into()
        }
        other => other,
    }
}
