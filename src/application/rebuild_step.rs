//! Rebuild after an edit, shared by the use cases that change the file.

use super::error::AppError;
use super::ports::{ManagedFile, Notice, Question, Services};

/// Offers the rebuild, unless `no_rebuild`. Declining keeps the edit. After a failed
/// rebuild, the user may go back to `original`.
pub fn run_after_edit(
    services: Services,
    file: &mut dyn ManagedFile,
    original: &str,
    no_rebuild: bool,
) -> Result<(), AppError> {
    if no_rebuild || !services.ui.confirm(&Question::RunRebuild)? {
        services.ui.notify(&Notice::RebuildSkipped)?;
        return Ok(());
    }
    if let Err(error) = services.rebuilder.rebuild() {
        services.ui.notify(&Notice::RebuildFailed)?;
        if services.ui.confirm(&Question::UndoEdit)? {
            file.replace(original)?;
            services.ui.notify(&Notice::EditUndone)?;
        }
        return Err(error.into());
    }
    services.ui.notify(&Notice::RebuildDone)?;
    Ok(())
}
