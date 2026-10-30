//! Prompts and messages on the terminal.

use std::cell::RefCell;
use std::io::{self, BufRead, Write};

use super::presenter;
use crate::application::ports::{Choice, Interaction, Notice, Question};
use crate::domain::package::Package;

/// Interaction channel with the user, generic so it can be exercised in tests.
pub struct Terminal<R, W> {
    input: RefCell<R>,
    output: RefCell<W>,
}

impl<R: BufRead, W: Write> Terminal<R, W> {
    /// Creates a terminal over the given input and output.
    pub fn new(input: R, output: W) -> Self {
        Self {
            input: RefCell::new(input),
            output: RefCell::new(output),
        }
    }

    fn say(&self, message: &str) -> io::Result<()> {
        writeln!(self.output.borrow_mut(), "{message}")
    }

    /// Asks a question and returns the answer without surrounding whitespace, or `None`
    /// if the input has ended.
    fn ask(&self, prompt: &str) -> io::Result<Option<String>> {
        {
            let mut output = self.output.borrow_mut();
            write!(output, "{prompt} ")?;
            output.flush()?;
        }
        let mut answer = String::new();
        if self.input.borrow_mut().read_line(&mut answer)? == 0 {
            return Ok(None);
        }
        Ok(Some(answer.trim().to_owned()))
    }
}

impl<R: BufRead, W: Write> Interaction for Terminal<R, W> {
    fn notify(&self, notice: &Notice) -> io::Result<()> {
        self.say(&presenter::notice(notice))
    }

    /// Only an explicit affirmative answer confirms; an empty or unknown answer, or the
    /// end of the input, counts as a refusal.
    fn confirm(&self, question: &Question) -> io::Result<bool> {
        let answer = self.ask(&format!("{} [y/N]", presenter::question(question)))?;
        Ok(answer.is_some_and(|answer| matches!(answer.to_lowercase().as_str(), "y" | "yes")))
    }

    fn ask_packages_path(&self) -> io::Result<Option<String>> {
        self.ask("Path to the packages.nix to manage:")
    }

    /// Any answer other than a listed number or a request for more, or the end of the
    /// input, counts as giving up.
    fn choose_package(
        &self,
        requested: &str,
        page: &[Package],
        first_index: usize,
        has_more: bool,
    ) -> io::Result<Choice> {
        if first_index == 0 && !page.is_empty() {
            self.say(&format!(
                "No exact match for '{requested}'. Similar packages:"
            ))?;
        }
        for (offset, package) in page.iter().enumerate() {
            self.say(&format!(
                "  {}) {}",
                first_index + offset + 1,
                presenter::one_line(package)
            ))?;
        }
        let count = first_index + page.len();
        let more = if has_more { ", m for more" } else { "" };
        let answer = self.ask(&format!(
            "Select a package [1-{count}{more}, Enter to cancel]"
        ))?;
        Ok(match answer.as_deref() {
            Some("m" | "M") if has_more => Choice::More,
            Some(text) => text
                .parse::<usize>()
                .ok()
                .filter(|choice| (1..=count).contains(choice))
                .map_or(Choice::Cancel, |choice| Choice::Index(choice - 1)),
            None => Choice::Cancel,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(input: &str) -> Terminal<&[u8], Vec<u8>> {
        Terminal::new(input.as_bytes(), Vec::new())
    }

    fn output(term: Terminal<&[u8], Vec<u8>>) -> String {
        String::from_utf8(term.output.into_inner()).unwrap()
    }

    #[test]
    fn confirm_accepts_only_explicit_answers() {
        for answer in ["y\n", "Y\n", "yes\n", " YES \n"] {
            assert!(
                terminal(answer).confirm(&Question::ApplyEdit).unwrap(),
                "{answer:?}"
            );
        }
        for answer in ["", "\n", "n\n", "no\n", "maybe\n", "yy\n", "s\n"] {
            assert!(
                !terminal(answer).confirm(&Question::ApplyEdit).unwrap(),
                "{answer:?}"
            );
        }
    }

    #[test]
    fn choose_returns_a_valid_index_or_gives_up() {
        assert_eq!(
            terminal("2\n").choose_package("x", &[], 3, false).unwrap(),
            Choice::Index(1)
        );
        for answer in ["", "\n", "0\n", "4\n", "-1\n", "abc\n", "m\n"] {
            assert_eq!(
                terminal(answer).choose_package("x", &[], 3, false).unwrap(),
                Choice::Cancel,
                "{answer:?}"
            );
        }
    }

    #[test]
    fn choose_offers_more_options_only_when_there_are_any() {
        assert_eq!(
            terminal("m\n").choose_package("x", &[], 3, true).unwrap(),
            Choice::More
        );
        let term = terminal("\n");
        term.choose_package("x", &[], 3, true).unwrap();
        assert_eq!(
            output(term),
            "Select a package [1-3, m for more, Enter to cancel] "
        );
    }

    #[test]
    fn ask_shows_the_prompt_and_distinguishes_end_of_input() {
        let term = terminal("  /a/b.nix \n");
        assert_eq!(
            term.ask_packages_path().unwrap().as_deref(),
            Some("/a/b.nix")
        );
        assert_eq!(term.ask_packages_path().unwrap(), None);
        assert!(output(term).starts_with("Path to the packages.nix to manage: "));
    }
}
