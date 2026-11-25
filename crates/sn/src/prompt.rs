//! Questions to the person at the terminal. Nothing here runs in agent mode
//! or without a terminal.

use sn_core::{Error, Result};

fn failed(err: dialoguer::Error) -> Error {
    Error::invalid(format!(
        "the question was not answered ({err}); pass the value as a flag"
    ))
}

/// Dialoguer reads the terminal synchronously.
fn blocking<T>(ask: impl FnOnce() -> dialoguer::Result<T>) -> Result<T> {
    tokio::task::block_in_place(ask).map_err(failed)
}

pub fn text(question: &str) -> Result<String> {
    blocking(|| {
        dialoguer::Input::new()
            .with_prompt(question)
            .interact_text()
    })
}

pub fn secret(question: &str) -> Result<String> {
    blocking(|| dialoguer::Password::new().with_prompt(question).interact())
}

pub fn confirm(question: &str) -> Result<bool> {
    blocking(|| {
        dialoguer::Confirm::new()
            .with_prompt(question)
            .default(false)
            .interact()
    })
}

/// The index of the chosen item.
pub fn choose(question: &str, items: &[String]) -> Result<usize> {
    blocking(|| {
        dialoguer::Select::new()
            .with_prompt(question)
            .items(items)
            .default(0)
            .interact()
    })
}
