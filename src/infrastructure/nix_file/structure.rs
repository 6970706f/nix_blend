//! Locates the package list of a `packages.nix`.
//!
//! The list must be written as `[ ... ]` or `with pkgs; [ ... ]`, with `pkgs`
//! bound by the file, and be one of:
//!
//! - A top-level module attribute: `environment.systemPackages`, `home.packages`
//!   or `users.users.<name>.packages`, optionally under `config`, spelled with
//!   dots or nested sets such as `home = { packages = ...; };`.
//! - The whole expression of the file.
//!
//! Both may follow function headers (including `@` patterns), `let ... in` and
//! `with ...;`. Any other form (`++`, `lib.mkIf`, more than one list) is refused:
//! guessing could change the meaning of the configuration.

use super::lexer::{Token, TokenKind};
use crate::application::error::EditError;

/// Package list found, as indices into the token vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListSite {
    pub open: usize,
    pub close: usize,
    /// The list is under `with pkgs;`, so names do not need the prefix.
    pub with_pkgs: bool,
}

/// `let` binding that imports another package set from a flake input, as in
/// `unstable = import inputs.nixpkgs-unstable { ... };`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelAlias<'a> {
    pub name: &'a str,
    pub input: &'a str,
}

fn unsupported(reason: impl Into<String>) -> EditError {
    EditError::UnsupportedStructure(reason.into())
}

/// Delimiters that form scopes. `let ... in` counts as a pair because the
/// bindings of a `let` are not attributes of the module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Brace,
    Bracket,
    Paren,
    Let,
}

/// View of the tokens together with the text they came from.
#[derive(Clone, Copy)]
pub struct Tokens<'a> {
    pub source: &'a str,
    pub tokens: &'a [Token],
}

impl<'a> Tokens<'a> {
    /// Text of the token at `index`.
    pub fn text(&self, index: usize) -> &'a str {
        let token = self.tokens[index];
        &self.source[token.start..token.end]
    }

    pub(super) fn is_punct(&self, index: usize, punct: u8) -> bool {
        self.tokens
            .get(index)
            .is_some_and(|token| token.kind == TokenKind::Punct(punct))
    }

    /// Whether the token at `index` exists and is an identifier.
    pub fn is_ident(&self, index: usize) -> bool {
        self.tokens
            .get(index)
            .is_some_and(|token| token.kind == TokenKind::Ident)
    }

    pub(super) fn is_word(&self, index: usize, word: &str) -> bool {
        self.is_ident(index) && self.text(index) == word
    }

    /// Scope opened or closed by the token, if any.
    fn scope_change(&self, index: usize) -> Option<(Scope, bool)> {
        match self.tokens[index].kind {
            TokenKind::Punct(b'{') => Some((Scope::Brace, true)),
            TokenKind::Punct(b'}') => Some((Scope::Brace, false)),
            TokenKind::Punct(b'[') => Some((Scope::Bracket, true)),
            TokenKind::Punct(b']') => Some((Scope::Bracket, false)),
            TokenKind::Punct(b'(') => Some((Scope::Paren, true)),
            TokenKind::Punct(b')') => Some((Scope::Paren, false)),
            TokenKind::Ident if self.text(index) == "let" => Some((Scope::Let, true)),
            TokenKind::Ident if self.text(index) == "in" => Some((Scope::Let, false)),
            _ => None,
        }
    }

    /// Index of the token that closes the scope opened at `open`.
    pub(super) fn matching_close(&self, open: usize) -> Result<usize, EditError> {
        let mut stack = Vec::new();
        for index in open..self.tokens.len() {
            match self.scope_change(index) {
                Some((scope, true)) => stack.push(scope),
                Some((scope, false)) => {
                    if stack.pop() != Some(scope) {
                        return Err(unbalanced());
                    }
                    if stack.is_empty() {
                        return Ok(index);
                    }
                }
                None => {}
            }
        }
        Err(unbalanced())
    }

    /// Reads `a.b.c` from `start`, returning the path and the following index.
    pub(super) fn dotted_path(&self, start: usize) -> Option<(String, usize)> {
        if !self.is_ident(start) {
            return None;
        }
        let mut path = self.text(start).to_owned();
        let mut index = start + 1;
        while self.is_punct(index, b'.') && self.is_ident(index + 1) {
            path.push('.');
            path.push_str(self.text(index + 1));
            index += 2;
        }
        Some((path, index))
    }

    /// Whether a binding can start at `index`, judging by the previous token.
    fn at_binding_position(&self, index: usize) -> bool {
        index == 0
            || self.is_punct(index - 1, b'{')
            || self.is_punct(index - 1, b';')
            || self.is_word(index - 1, "let")
    }
}

fn unbalanced() -> EditError {
    unsupported("the file's delimiters are not balanced")
}

/// Locates the single package list of the file.
pub fn locate(tokens: Tokens) -> Result<ListSite, EditError> {
    let (body, outer_with_pkgs) = skip_preamble(tokens)?;
    let site = if tokens.is_punct(body, b'[') || tokens.is_word(body, "with") {
        let site = parse_list(tokens, body)?;
        if site.close + 1 != tokens.tokens.len() {
            return Err(combined_list());
        }
        site
    } else {
        let mut site = locate_attribute(tokens)?;
        site.with_pkgs |= outer_with_pkgs;
        site
    };
    require_pkgs_scope(tokens, site)
}

/// Skips function headers, `let ... in` and `with ...;`, stopping at the start of
/// the body. Also tells whether the innermost `with` skipped was `with pkgs;`.
fn skip_preamble(tokens: Tokens) -> Result<(usize, bool), EditError> {
    let mut index = 0;
    let mut with_pkgs = false;
    loop {
        if tokens.is_punct(index, b'{') {
            let close = tokens.matching_close(index)?;
            if tokens.is_punct(close + 1, b':') {
                index = close + 2;
            } else if tokens.is_punct(close + 1, b'@')
                && tokens.is_ident(close + 2)
                && tokens.is_punct(close + 3, b':')
            {
                index = close + 4;
            } else {
                return Ok((index, with_pkgs));
            }
        } else if tokens.is_ident(index) && tokens.is_punct(index + 1, b':') {
            index += 2;
        } else if tokens.is_ident(index)
            && tokens.is_punct(index + 1, b'@')
            && tokens.is_punct(index + 2, b'{')
        {
            let close = tokens.matching_close(index + 2)?;
            if !tokens.is_punct(close + 1, b':') {
                return Ok((index, with_pkgs));
            }
            index = close + 2;
        } else if tokens.is_word(index, "let") {
            index = tokens.matching_close(index)? + 1;
        } else if tokens.is_word(index, "with") {
            // A `with` right before the list belongs to the list itself.
            let Some((path, next)) = tokens.dotted_path(index + 1) else {
                return Ok((index, with_pkgs));
            };
            if !tokens.is_punct(next, b';') || tokens.is_punct(next + 1, b'[') {
                return Ok((index, with_pkgs));
            }
            with_pkgs = path == "pkgs";
            index = next + 1;
        } else {
            return Ok((index, with_pkgs));
        }
    }
}

/// Scope being walked. `prefix` is the attribute path of a set known to be part
/// of the module's top level, such as `home` in `home = { ... };`.
struct Frame {
    scope: Scope,
    prefix: Option<String>,
}

struct Candidate {
    path: String,
    equals: usize,
    top_level: bool,
}

/// Searches the whole file for the known attributes. Occurrences at any depth
/// count, so that a nested list makes the case ambiguous instead of being ignored.
fn locate_attribute(tokens: Tokens) -> Result<ListSite, EditError> {
    let mut frames: Vec<Frame> = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();

    for index in 0..tokens.tokens.len() {
        match tokens.scope_change(index) {
            Some((scope, true)) => {
                let prefix = (scope == Scope::Brace)
                    .then(|| set_prefix(tokens, &frames, index))
                    .flatten();
                frames.push(Frame { scope, prefix });
            }
            Some((scope, false)) => {
                let closed = frames.pop().map(|frame| frame.scope);
                if closed != Some(scope) {
                    return Err(unbalanced());
                }
            }
            None if tokens.is_ident(index) && tokens.at_binding_position(index) => {
                if let Some(candidate) = target_binding(tokens, &frames, index) {
                    candidates.push(candidate);
                }
            }
            None => {}
        }
    }
    if !frames.is_empty() {
        return Err(unbalanced());
    }

    match candidates.as_slice() {
        [] => Err(unsupported(
            "no package list recognized. Supported formats: 'environment.systemPackages', 'home.packages' or 'users.users.<name>.packages' at the top level of the module, or a file that is only the list, using '[ ... ]' or 'with pkgs; [ ... ]'",
        )),
        [candidate] => {
            if !candidate.top_level {
                return Err(unsupported(format!(
                    "'{}' is not at the top level of the module",
                    candidate.path
                )));
            }
            let site = parse_list(tokens, candidate.equals + 1)?;
            if tokens.is_punct(site.close + 1, b';') {
                Ok(site)
            } else {
                Err(combined_list())
            }
        }
        many => Err(unsupported(format!(
            "the file defines {} package lists; cannot safely choose one",
            many.len()
        ))),
    }
}

/// Attribute path of the set opened at `open`, if it belongs to the module's top
/// level. A set passed as an argument, as in `lib.mkIf x { ... }`, does not.
fn set_prefix(tokens: Tokens, frames: &[Frame], open: usize) -> Option<String> {
    // `rec { }` is a set like any other, for where it may appear.
    let open = if open > 0 && tokens.is_word(open - 1, "rec") {
        open - 1
    } else {
        open
    };
    let Some(parent) = frames.last() else {
        let is_body = open == 0
            || tokens.is_punct(open - 1, b':')
            || tokens.is_punct(open - 1, b';')
            || tokens.is_word(open - 1, "in");
        return is_body.then(String::new);
    };
    let parent_prefix = parent.prefix.as_deref()?;
    if open < 2 || !tokens.is_punct(open - 1, b'=') {
        return None;
    }
    let mut start = open - 2;
    while start >= 2 && tokens.is_punct(start - 1, b'.') && tokens.is_ident(start - 2) {
        start -= 2;
    }
    if !tokens.at_binding_position(start) {
        return None;
    }
    let (path, _) = tokens.dotted_path(start)?;
    Some(join_path(parent_prefix, &path))
}

fn join_path(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        path.to_owned()
    } else {
        format!("{prefix}.{path}")
    }
}

/// Binding starting at `index`, if it assigns one of the known attributes. Inside
/// a set of unknown position, the path alone decides.
fn target_binding(tokens: Tokens, frames: &[Frame], index: usize) -> Option<Candidate> {
    let (path, equals) = tokens.dotted_path(index)?;
    if !tokens.is_punct(equals, b'=') {
        return None;
    }
    let prefix = frames.last().and_then(|frame| frame.prefix.as_deref());
    let path = join_path(prefix.unwrap_or(""), &path);
    is_target_path(&path).then_some(Candidate {
        path,
        equals,
        top_level: prefix.is_some(),
    })
}

fn is_target_path(path: &str) -> bool {
    let path = path.strip_prefix("config.").unwrap_or(path);
    if matches!(path, "environment.systemPackages" | "home.packages") {
        return true;
    }
    path.strip_prefix("users.users.")
        .and_then(|rest| rest.strip_suffix(".packages"))
        .is_some_and(|name| !name.contains('.'))
}

/// Interprets `[ ... ]` or `with pkgs; [ ... ]` starting at `start`.
fn parse_list(tokens: Tokens, start: usize) -> Result<ListSite, EditError> {
    let with_pkgs = tokens.is_word(start, "with");
    let open = if with_pkgs {
        if !tokens.is_word(start + 1, "pkgs") || !tokens.is_punct(start + 2, b';') {
            return Err(unsupported("only 'with pkgs;' is accepted before the list"));
        }
        start + 3
    } else {
        start
    };
    if !tokens.is_punct(open, b'[') {
        return Err(unsupported(
            "the attribute value is not a plain list ('[ ... ]' or 'with pkgs; [ ... ]')",
        ));
    }
    Ok(ListSite {
        open,
        close: tokens.matching_close(open)?,
        with_pkgs,
    })
}

fn combined_list() -> EditError {
    unsupported(
        "the package list is part of a larger expression (for example, with '++'); cannot tell where to add the package",
    )
}

/// Without `pkgs` bound by the file, neither `pkgs.name` nor `with pkgs;` can
/// evaluate; the Nix parser itself rejects the second form.
fn require_pkgs_scope(tokens: Tokens, site: ListSite) -> Result<ListSite, EditError> {
    let bound = (0..site.open).any(|index| {
        tokens.is_word(index, "pkgs")
            && (index == 0
                || !(tokens.is_word(index - 1, "with") || tokens.is_punct(index - 1, b'.')))
    });
    if !bound {
        return Err(unsupported("'pkgs' is not in scope before the list"));
    }
    Ok(site)
}

/// Whether `name` may be bound by the file before the list. A `let` or function
/// argument takes precedence over `with pkgs;` in Nix, so such a name must be
/// written as `pkgs.name`. A false positive only makes the entry more explicit.
pub fn may_shadow(tokens: Tokens, site: ListSite, name: &str) -> bool {
    (0..site.open).any(|index| {
        tokens.is_word(index, name)
            && (index == 0
                || !(tokens.is_word(index - 1, "with") || tokens.is_punct(index - 1, b'.')))
    })
}

/// Finds the bindings that import another package set from a flake input.
pub fn channel_aliases<'a>(tokens: Tokens<'a>) -> Vec<ChannelAlias<'a>> {
    (0..tokens.tokens.len())
        .filter_map(|index| channel_alias_at(tokens, index))
        .collect()
}

fn channel_alias_at<'a>(tokens: Tokens<'a>, index: usize) -> Option<ChannelAlias<'a>> {
    if !tokens.is_ident(index)
        || !tokens.at_binding_position(index)
        || !tokens.is_punct(index + 1, b'=')
        || tokens.text(index) == "pkgs"
    {
        return None;
    }
    let mut value = index + 2;
    let imported = tokens.is_word(value, "import");
    if imported {
        value += 1;
    }
    if tokens.is_word(value, "inputs") && tokens.is_punct(value + 1, b'.') {
        value += 2;
    }
    if !tokens.is_ident(value) {
        return None;
    }
    let is_package_set = if imported {
        tokens.is_punct(value + 1, b'{')
    } else {
        tokens.is_punct(value + 1, b'.') && tokens.is_word(value + 2, "legacyPackages")
    };
    is_package_set.then(|| ChannelAlias {
        name: tokens.text(index),
        input: tokens.text(value),
    })
}
