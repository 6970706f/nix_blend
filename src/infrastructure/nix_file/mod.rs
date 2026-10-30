//! Reading and editing package files.
//!
//! This is not a parser for the Nix language. It delimits tokens as the language does
//! and recognizes the structures listed in [`structure`], refusing anything else. The
//! real Nix parser validates every result before it is written.

mod elements;
mod lexer;
mod structure;

use structure::{ListSite, Tokens};

use crate::application::error::EditError;
use crate::application::ports::PackageDocument;
use crate::domain::package::{
    ChannelAlias, DeclaredPackages, PackageDeclaration, PackageIdentity, PackageName, PackageSource,
};

const PKGS_PREFIX: &str = "pkgs.";
const DEFAULT_INDENT: &str = "  ";

/// [`PackageDocument`] for the supported Nix structures.
#[derive(Debug, Clone, Copy, Default)]
pub struct NixPackageDocument;

fn tokenize(source: &str) -> Result<Vec<lexer::Token>, EditError> {
    lexer::tokenize(source).map_err(|reason| EditError::UnsupportedStructure(reason.to_owned()))
}

impl PackageDocument for NixPackageDocument {
    fn analyze(&self, source: &str) -> Result<DeclaredPackages, EditError> {
        let tokens = tokenize(source)?;
        let view = Tokens {
            source,
            tokens: &tokens,
        };
        let site = structure::locate(view)?;
        let aliases = structure::channel_aliases(view);
        Ok(DeclaredPackages {
            declarations: elements::declarations(view, site, &aliases),
            aliases: aliases
                .iter()
                .map(|alias| ChannelAlias {
                    name: alias.name.to_owned(),
                    input: alias.input.to_owned(),
                })
                .collect(),
        })
    }

    fn add(&self, source: &str, identity: &PackageIdentity) -> Result<String, EditError> {
        // The last line of defense: whatever the source of this name, it becomes Nix code.
        if PackageName::parse(&identity.attr).is_err() {
            return Err(EditError::InvalidAttribute(identity.attr.clone()));
        }
        let tokens = tokenize(source)?;
        let view = Tokens {
            source,
            tokens: &tokens,
        };
        let site = structure::locate(view)?;
        let entry = match &identity.source {
            PackageSource::Channel { alias, .. } => format!("{alias}.{}", identity.attr),
            _ => {
                let first_segment = identity.attr.split('.').next().unwrap_or(&identity.attr);
                if site.with_pkgs && !structure::may_shadow(view, site, first_segment) {
                    identity.attr.clone()
                } else {
                    format!("{PKGS_PREFIX}{}", identity.attr)
                }
            }
        };
        Ok(insert_entry(view, site, &entry))
    }

    fn remove(&self, source: &str, declaration: &PackageDeclaration) -> Result<String, EditError> {
        let before = self.analyze(source)?;
        let new_content = cut_entry(source, declaration)?;

        // Only that entry may disappear. Anything else means this edit misread the file.
        let after = self.analyze(&new_content)?;
        let remaining = |declared: &DeclaredPackages| -> Vec<String> {
            declared
                .declarations
                .iter()
                .map(|entry| entry.text.clone())
                .collect()
        };
        let mut expected = remaining(&before);
        let position = before
            .declarations
            .iter()
            .position(|entry| entry == declaration)
            .ok_or_else(|| removal_failure(declaration))?;
        expected.remove(position);
        if remaining(&after) != expected {
            return Err(removal_failure(declaration));
        }
        Ok(new_content)
    }
}

fn removal_failure(declaration: &PackageDeclaration) -> EditError {
    EditError::UnsupportedStructure(format!(
        "could not isolate the entry '{}' in the list",
        declaration.text
    ))
}

/// Removes an entry. When it is alone on its lines, the lines go too, with a comment
/// that follows it on the same line, since that comment describes the entry. Comments
/// on other lines stay.
fn cut_entry(source: &str, declaration: &PackageDeclaration) -> Result<String, EditError> {
    let (start, end) = (declaration.start, declaration.end);
    if start > end || end > source.len() || source.get(start..end) != Some(&declaration.text) {
        return Err(removal_failure(declaration));
    }
    let line_start = line_start_of(source, start);
    let line_end = source[end..].find('\n').map_or(source.len(), |at| end + at);
    let before = &source[line_start..start];
    let after = &source[end..line_end];
    let alone_before = before.bytes().all(is_indent_byte);
    let trimmed_after = after.trim();

    let mut result = String::with_capacity(source.len());
    if alone_before && (trimmed_after.is_empty() || trimmed_after.starts_with('#')) {
        result.push_str(&source[..line_start]);
        result.push_str(source.get(line_end + 1..).unwrap_or(""));
    } else if alone_before {
        let following = after.len() - after.trim_start_matches([' ', '\t']).len();
        result.push_str(&source[..start]);
        result.push_str(&source[end + following..]);
    } else {
        result.push_str(source[..start].trim_end_matches([' ', '\t']));
        result.push_str(&source[end..]);
    }
    Ok(result)
}

/// Inserts the entry following the list's style: on its own line when the `]` is alone
/// on its line and, otherwise, on the same line as the `]`.
fn insert_entry(tokens: Tokens, site: ListSite, entry: &str) -> String {
    let source = tokens.source;
    let close = tokens.tokens[site.close].start;
    let line_start = line_start_of(source, close);
    let closing_indent = &source[line_start..close];

    let mut result = String::with_capacity(source.len() + entry.len() + 16);
    if closing_indent.bytes().all(is_indent_byte) {
        let line_ending = if source[..line_start].ends_with("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let indent = last_entry_indent(tokens, site).map_or_else(
            || format!("{closing_indent}{DEFAULT_INDENT}"),
            str::to_owned,
        );
        result.push_str(&source[..line_start]);
        result.push_str(&indent);
        result.push_str(entry);
        result.push_str(line_ending);
        result.push_str(&source[line_start..]);
    } else {
        result.push_str(source[..close].trim_end_matches([' ', '\t']));
        result.push(' ');
        result.push_str(entry);
        result.push(' ');
        result.push_str(&source[close..]);
    }
    result
}

/// Indentation of the line holding the last token of the list, or `None` if the list is
/// empty or that line is the one with `[`, which is not a useful reference.
fn last_entry_indent<'a>(tokens: Tokens<'a>, site: ListSite) -> Option<&'a str> {
    if site.close == site.open + 1 {
        return None;
    }
    let source = tokens.source;
    let line_start = line_start_of(source, tokens.tokens[site.close - 1].start);
    if line_start <= tokens.tokens[site.open].start {
        return None;
    }
    let line = &source[line_start..];
    let length = line
        .bytes()
        .take_while(|byte| is_indent_byte(*byte))
        .count();
    Some(&line[..length])
}

fn line_start_of(source: &str, position: usize) -> usize {
    source[..position].rfind('\n').map_or(0, |at| at + 1)
}

fn is_indent_byte(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t')
}

#[cfg(test)]
mod tests;
