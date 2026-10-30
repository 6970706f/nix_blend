//! Minimal Nix lexer. It recognizes only what is needed to locate lists safely:
//! comments, strings and interpolations must not be mistaken for code, and
//! identifiers, paths and URIs must be delimited as in the language grammar.

/// Limit on nested interpolations, so that a pathological input produces an error
/// instead of overflowing the stack.
const MAX_INTERPOLATION_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Ident,
    Punct(u8),
    Str,
    Other,
}

/// Token with its byte position in the original text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
}

/// Splits `source` into tokens. Returns a description of the problem if the text
/// cannot be delimited safely.
pub fn tokenize(source: &str) -> Result<Vec<Token>, &'static str> {
    let mut lexer = Lexer {
        bytes: source.as_bytes(),
        pos: 0,
        depth: 0,
        tokens: Vec::new(),
    };
    lexer.code(false)?;
    Ok(lexer.tokens)
}

struct Lexer<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
    tokens: Vec<Token>,
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'\'' | b'-')
}

fn is_path_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+')
}

fn is_uri_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'%' | b'/'
                | b'?'
                | b':'
                | b'@'
                | b'&'
                | b'='
                | b'+'
                | b'$'
                | b','
                | b'-'
                | b'_'
                | b'.'
                | b'!'
                | b'~'
                | b'*'
                | b'\''
        )
}

impl Lexer<'_> {
    fn peek(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        self.tokens.push(Token {
            kind,
            start,
            end: self.pos,
        });
    }

    fn single(&mut self, kind: TokenKind) {
        let start = self.pos;
        self.pos += 1;
        self.push(kind, start);
    }

    /// Consumes code up to the end of the text or, in an interpolation, up to
    /// the `}` that closes it.
    fn code(&mut self, in_interpolation: bool) -> Result<(), &'static str> {
        let mut open_braces = 0usize;
        while let Some(byte) = self.peek(0) {
            match byte {
                b' ' | b'\t' | b'\r' | b'\n' => self.pos += 1,
                b'#' => self.line_comment(),
                b'/' if self.peek(1) == Some(b'*') => self.block_comment()?,
                b'"' => self.string()?,
                b'\'' if self.peek(1) == Some(b'\'') => self.indented_string()?,
                b'{' => {
                    open_braces += 1;
                    self.single(TokenKind::Punct(byte));
                }
                b'}' => {
                    if open_braces == 0 && in_interpolation {
                        self.pos += 1;
                        return Ok(());
                    }
                    open_braces = open_braces.saturating_sub(1);
                    self.single(TokenKind::Punct(byte));
                }
                b'[' | b']' | b'(' | b')' | b';' | b':' | b'@' => {
                    self.single(TokenKind::Punct(byte));
                }
                b'<' if self.search_path_length().is_some() => {
                    let length = self.search_path_length().unwrap_or(1);
                    self.operator(length);
                }
                b'=' | b'!' | b'<' | b'>' if self.peek(1) == Some(b'=') => self.operator(2),
                b'=' => self.single(TokenKind::Punct(byte)),
                b'.' => self.dot(),
                b'~' if self.peek(1) == Some(b'/') => self.path(),
                b'/' if self.peek(1).is_some_and(is_path_char) => self.path(),
                b'/' if self.peek(1) == Some(b'/') => self.operator(2),
                b'+' if self.peek(1) == Some(b'+') => self.operator(2),
                _ if is_ident_start(byte) => self.word(),
                _ if byte.is_ascii_digit() => self.path(),
                _ => self.single(TokenKind::Other),
            }
        }
        if in_interpolation {
            return Err("unterminated interpolation");
        }
        Ok(())
    }

    /// Length of a search path such as `<nixpkgs/lib>` at the current position.
    fn search_path_length(&self) -> Option<usize> {
        let mut length = 1;
        while let Some(byte) = self.peek(length) {
            if byte == b'>' {
                return (length > 1).then_some(length + 1);
            }
            if !is_path_char(byte) && byte != b'/' {
                return None;
            }
            length += 1;
        }
        None
    }

    fn operator(&mut self, length: usize) {
        let start = self.pos;
        self.pos += length;
        self.push(TokenKind::Other, start);
    }

    fn line_comment(&mut self) {
        while self.peek(0).is_some_and(|byte| byte != b'\n') {
            self.pos += 1;
        }
    }

    fn block_comment(&mut self) -> Result<(), &'static str> {
        self.pos += 2;
        while let Some(byte) = self.peek(0) {
            if byte == b'*' && self.peek(1) == Some(b'/') {
                self.pos += 2;
                return Ok(());
            }
            self.pos += 1;
        }
        Err("unterminated comment")
    }

    /// A dot can start a path (`./a`, `../a`), be an ellipsis (`...`) or separate
    /// attributes.
    fn dot(&mut self) {
        match (self.peek(1), self.peek(2)) {
            (Some(b'/'), _) | (Some(b'.'), Some(b'/')) => self.path(),
            (Some(b'.'), Some(b'.')) => self.operator(3),
            _ => self.single(TokenKind::Punct(b'.')),
        }
    }

    /// Paths and numbers: neither matters beyond its boundaries.
    fn path(&mut self) {
        let start = self.pos;
        self.consume_path_chars();
        self.push(TokenKind::Other, start);
    }

    fn consume_path_chars(&mut self) {
        while let Some(byte) = self.peek(0) {
            let continues = is_path_char(byte)
                || (matches!(byte, b'/' | b'~') && self.peek(1).is_some_and(is_path_char));
            if !continues {
                break;
            }
            self.pos += 1;
        }
    }

    /// An identifier, or a path (`a/b`) or URI (`https://x`) that starts like one.
    /// This also avoids treating the `''` of `foo''` as the start of a string.
    fn word(&mut self) {
        let start = self.pos;
        while self.peek(0).is_some_and(is_ident_char) {
            self.pos += 1;
        }
        match (self.peek(0), self.peek(1)) {
            (Some(b'/'), Some(next)) if is_path_char(next) => {
                self.consume_path_chars();
                self.push(TokenKind::Other, start);
            }
            (Some(b':'), Some(next)) if is_uri_char(next) => {
                while self.peek(0).is_some_and(is_uri_char) {
                    self.pos += 1;
                }
                self.push(TokenKind::Other, start);
            }
            _ => self.push(TokenKind::Ident, start),
        }
    }

    fn string(&mut self) -> Result<(), &'static str> {
        let start = self.pos;
        self.pos += 1;
        while let Some(byte) = self.peek(0) {
            match (byte, self.peek(1)) {
                (b'"', _) => {
                    self.pos += 1;
                    self.push(TokenKind::Str, start);
                    return Ok(());
                }
                (b'\\', _) | (b'$', Some(b'$')) => self.pos += 2,
                (b'$', Some(b'{')) => self.interpolation()?,
                _ => self.pos += 1,
            }
        }
        Err("unterminated string")
    }

    fn indented_string(&mut self) -> Result<(), &'static str> {
        let start = self.pos;
        self.pos += 2;
        while let Some(byte) = self.peek(0) {
            match (byte, self.peek(1), self.peek(2)) {
                (b'\'', Some(b'\''), Some(b'\\')) => self.pos += 4,
                (b'\'', Some(b'\''), Some(b'$' | b'\'')) => self.pos += 3,
                (b'\'', Some(b'\''), _) => {
                    self.pos += 2;
                    self.push(TokenKind::Str, start);
                    return Ok(());
                }
                (b'$', Some(b'$'), _) => self.pos += 2,
                (b'$', Some(b'{'), _) => self.interpolation()?,
                _ => self.pos += 1,
            }
        }
        Err("unterminated indented string")
    }

    /// Consumes `${ ... }` inside a string, discarding the inner tokens.
    fn interpolation(&mut self) -> Result<(), &'static str> {
        if self.depth == MAX_INTERPOLATION_DEPTH {
            return Err("too many nested interpolations");
        }
        self.pos += 2;
        self.depth += 1;
        let token_count = self.tokens.len();
        self.code(true)?;
        self.tokens.truncate(token_count);
        self.depth -= 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(source: &str) -> Vec<&str> {
        tokenize(source)
            .unwrap()
            .iter()
            .map(|token| &source[token.start..token.end])
            .collect()
    }

    #[test]
    fn splits_attribute_paths_and_punctuation() {
        assert_eq!(
            texts("home.packages = [ a ];"),
            ["home", ".", "packages", "=", "[", "a", "]", ";"]
        );
    }

    #[test]
    fn ignores_comments() {
        assert_eq!(texts("a # [ b\n/* [ c\n */ d"), ["a", "d"],);
    }

    #[test]
    fn keeps_strings_whole_even_with_interpolation() {
        assert_eq!(
            texts(r#"[ "a ] ${ f "}" { x = 1; } } \" b" ]"#),
            ["[", r#""a ] ${ f "}" { x = 1; } } \" b""#, "]"]
        );
    }

    #[test]
    fn handles_indented_string_escapes() {
        let source = "x ''a ''${ b ''' ''\\n $${ c ${ \"d\" } ] '' y";
        let tokens = texts(source);
        assert_eq!(tokens.first(), Some(&"x"));
        assert_eq!(tokens.last(), Some(&"y"));
        assert_eq!(tokens.len(), 3);
    }

    #[test]
    fn identifiers_may_end_with_quotes() {
        assert_eq!(texts("foo'' bar"), ["foo''", "bar"]);
    }

    #[test]
    fn hyphenated_identifiers_are_one_token() {
        assert_eq!(texts("yt-dlp"), ["yt-dlp"]);
    }

    #[test]
    fn paths_and_uris_do_not_open_comments() {
        assert_eq!(texts("./a/b.nix c"), ["./a/b.nix", "c"]);
        assert_eq!(texts("https://x.y/*z c"), ["https://x.y/*z", "c"]);
        assert_eq!(texts("a/b c"), ["a/b", "c"]);
    }

    #[test]
    fn search_paths_are_one_token() {
        assert_eq!(
            texts("[ <nixpkgs> <a/b.c> ]"),
            ["[", "<nixpkgs>", "<a/b.c>", "]"]
        );
        assert_eq!(texts("a < b"), ["a", "<", "b"]);
        assert_eq!(texts("a <= b"), ["a", "<=", "b"]);
    }

    #[test]
    fn lambda_colon_is_punctuation_when_followed_by_space() {
        assert_eq!(texts("pkgs: x"), ["pkgs", ":", "x"]);
    }

    #[test]
    fn distinguishes_equals_from_comparison() {
        let tokens = tokenize("a == b").unwrap();
        assert_eq!(tokens[1].kind, TokenKind::Other);
        let tokens = tokenize("a = b").unwrap();
        assert_eq!(tokens[1].kind, TokenKind::Punct(b'='));
    }

    #[test]
    fn distinguishes_ellipsis_from_dot() {
        assert_eq!(texts("{ pkgs, ... }:").len(), 6);
    }

    #[test]
    fn reports_unterminated_constructs() {
        assert!(tokenize("/* a").is_err());
        assert!(tokenize("\"a").is_err());
        assert!(tokenize("''a").is_err());
        assert!(tokenize("\"${ a\"").is_err());
    }

    #[test]
    fn rejects_excessive_nesting_without_overflowing_the_stack() {
        let source = "\"${".repeat(10_000);
        assert!(tokenize(&source).is_err());
    }
}
