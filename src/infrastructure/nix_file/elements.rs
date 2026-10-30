//! Splits a package list into its entries.

use super::lexer::TokenKind;
use super::structure::{ChannelAlias, ListSite, Tokens};
use crate::domain::package::{DeclarationKind, PackageDeclaration, PackageIdentity, PackageSource};

const OVERRIDE_SUFFIXES: [&str; 3] = [".override", ".overrideAttrs", ".overridePythonAttrs"];
const PKGS_PREFIX: &str = "pkgs.";
const INPUTS_PREFIX: &str = "inputs.";

/// Reads the entries of the list, in order. Each one spans all of its tokens, as in
/// `inputs.x.packages.${system}.default` or `(foo.override { })`.
pub fn declarations(
    tokens: Tokens,
    site: ListSite,
    aliases: &[ChannelAlias],
) -> Vec<PackageDeclaration> {
    let mut found = Vec::new();
    let mut index = site.open + 1;
    while index < site.close {
        let Some(last) = element_end(tokens, index, site.close) else {
            break;
        };
        found.push(classify(tokens, site, aliases, index, last));
        index = last + 1;
    }
    found
}

/// Index of the last token of the element starting at `first`. List elements are
/// separated only by whitespace, so an element is a primary expression followed by
/// attribute selections and any number of `or` fallbacks.
fn element_end(tokens: Tokens, first: usize, close: usize) -> Option<usize> {
    let mut last = primary_end(tokens, first)?;
    loop {
        if tokens.is_punct(last + 1, b'.') && last + 2 < close {
            let segment = last + 2;
            if is_dollar(tokens, segment) {
                last = tokens.matching_close(segment + 1).ok()?;
            } else {
                last = segment;
            }
        } else if tokens.is_word(last + 1, "or") && last + 2 < close {
            last = primary_end(tokens, last + 2)?;
        } else {
            return Some(last);
        }
    }
}

fn is_dollar(tokens: Tokens, index: usize) -> bool {
    tokens
        .tokens
        .get(index)
        .is_some_and(|token| token.kind == TokenKind::Other)
        && tokens.text(index) == "$"
}

/// A primary expression. `rec { }` is one, and a path with interpolations is too, as
/// its parts have no space between them: `./a${b}/c`.
fn primary_end(tokens: Tokens, first: usize) -> Option<usize> {
    let token = tokens.tokens.get(first)?;
    match token.kind {
        TokenKind::Punct(b'(' | b'[' | b'{') => tokens.matching_close(first).ok(),
        TokenKind::Ident if tokens.is_word(first, "rec") => tokens.matching_close(first + 1).ok(),
        TokenKind::Other => {
            let mut last = first;
            loop {
                let end = tokens.tokens[last].end;
                let adjacent = |index: usize| {
                    tokens
                        .tokens
                        .get(index)
                        .is_some_and(|next| next.start == end)
                };
                if is_dollar(tokens, last + 1) && adjacent(last + 1) {
                    last = tokens.matching_close(last + 2).ok()?;
                } else if adjacent(last + 1) && tokens.tokens[last + 1].kind == TokenKind::Other {
                    last += 1;
                } else {
                    return Some(last);
                }
            }
        }
        _ => Some(first),
    }
}

fn classify(
    tokens: Tokens,
    site: ListSite,
    aliases: &[ChannelAlias],
    first: usize,
    last: usize,
) -> PackageDeclaration {
    let (start, end) = (tokens.tokens[first].start, tokens.tokens[last].end);
    let (identity, kind) = if tokens.is_punct(first, b'(') {
        parenthesized(tokens, site, aliases, first)
    } else {
        match (!tokens.is_word(first, "rec"))
            .then(|| tokens.dotted_path(first))
            .flatten()
        {
            Some((path, next)) => {
                let kind = if next == last + 1 {
                    DeclarationKind::Plain
                } else {
                    DeclarationKind::Expression
                };
                (Some(identity_of(&path, site, aliases)), kind)
            }
            None => (None, DeclarationKind::Opaque),
        }
    };
    let (identity, kind) = match (identity, kind) {
        // A flake input path is one entry whatever its form, as in
        // `inputs.x.packages.${system}.default`.
        (Some(identity), _) if matches!(identity.source, PackageSource::FlakeInput(_)) => {
            (Some(identity), DeclarationKind::Plain)
        }
        other => other,
    };
    PackageDeclaration {
        identity,
        kind,
        text: tokens.source[start..end].to_owned(),
        line: tokens.source[..start].matches('\n').count() + 1,
        start,
        end,
    }
}

/// `(foo.override { })` is `foo` with changes, while `(python3.withPackages (ps: [ ]))`
/// is another thing based on an attribute.
fn parenthesized(
    tokens: Tokens,
    site: ListSite,
    aliases: &[ChannelAlias],
    open: usize,
) -> (Option<PackageIdentity>, DeclarationKind) {
    let Some((path, _)) = tokens.dotted_path(open + 1) else {
        return (None, DeclarationKind::Opaque);
    };
    let overridden = OVERRIDE_SUFFIXES
        .iter()
        .find_map(|suffix| path.strip_suffix(suffix));
    match overridden {
        Some(base) => (
            Some(identity_of(base, site, aliases)),
            DeclarationKind::Override,
        ),
        None => (
            Some(identity_of(&path, site, aliases)),
            DeclarationKind::Expression,
        ),
    }
}

/// Where an attribute path comes from. In a list without `with pkgs;`, a bare name is
/// something the file itself binds.
fn identity_of(path: &str, site: ListSite, aliases: &[ChannelAlias]) -> PackageIdentity {
    if let Some(attr) = path.strip_prefix(PKGS_PREFIX) {
        return PackageIdentity::nixpkgs(attr);
    }
    if let Some(rest) = path.strip_prefix(INPUTS_PREFIX) {
        let input = rest.split('.').next().unwrap_or(rest);
        return PackageIdentity {
            source: PackageSource::FlakeInput(input.to_owned()),
            attr: input.to_owned(),
        };
    }
    let channel = path.split_once('.').and_then(|(first, attr)| {
        let alias = aliases.iter().find(|alias| alias.name == first)?;
        Some(PackageIdentity {
            source: PackageSource::Channel {
                alias: alias.name.to_owned(),
                input: alias.input.to_owned(),
            },
            attr: attr.to_owned(),
        })
    });
    channel.unwrap_or_else(|| {
        if site.with_pkgs {
            PackageIdentity::nixpkgs(path)
        } else {
            PackageIdentity {
                source: PackageSource::Unknown,
                attr: path.to_owned(),
            }
        }
    })
}
