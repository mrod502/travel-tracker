//! Tokens produced by the lexer and the position they came from.

use std::fmt;
use std::path::PathBuf;

use crate::model::Statement;

/// A position in a migration file.
///
/// `line` and `col` are 1-based, counting characters (not bytes) from the start
/// of the line, so a message printed from a span lines up with what an editor
/// shows. `file` may be empty — [`Span`]'s [`Display`](std::fmt::Display) impl
/// prints `<input>` in that case.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Span {
    /// The file the text came from. May be empty for in-memory sources.
    pub file: PathBuf,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column, counted in characters.
    pub col: usize,
}

impl Span {
    /// A span at `line`/`col` in `file`.
    pub fn new(file: impl Into<PathBuf>, line: usize, col: usize) -> Self {
        Self {
            file: file.into(),
            line,
            col,
        }
    }

    /// A span for a source that has no file on disk, e.g. a string in a test.
    pub fn in_memory(line: usize, col: usize) -> Self {
        Self {
            file: PathBuf::new(),
            line,
            col,
        }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.file.as_os_str().is_empty() {
            write!(f, "<input>:{}:{}", self.line, self.col)
        } else {
            write!(f, "{}:{}:{}", self.file.display(), self.line, self.col)
        }
    }
}

/// A `--migrate:` comment as the lexer read it, before anyone knows what it
/// means.
///
/// The lexer validates only the shape of a directive (`path.name` segments, a
/// balanced parenthesised argument list). Which keys exist and what their
/// arguments mean is the [`DirectiveRegistry`](crate::directive::registry::DirectiveRegistry)'s
/// business.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDirective<'a> {
    /// The dot-separated path, e.g. `["up", "begin"]` or `["skipTx"]`.
    pub path: Vec<&'a str>,
    /// Raw text between the parentheses, if the directive was written with an
    /// argument list. Handed to
    /// [`parse_args`](crate::directive::args::parse_args) by the registry.
    pub args: Option<&'a str>,
    /// Where the `--` that opens the directive is.
    pub span: Span,
}

impl RawDirective<'_> {
    /// The dotted key, e.g. `up.begin`.
    pub fn key(&self) -> String {
        self.path.join(".")
    }
}

/// One unit of lexed migration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexToken<'a> {
    /// A complete SQL statement. `options` is empty here — only the parser
    /// knows which pending directive options apply.
    Statement(Statement),
    /// A `--migrate:` comment, lifted out of the SQL text.
    Directive(RawDirective<'a>),
}

/// The path text of a directive (`up.begin`) split into segments.
///
/// Returns `None` when any segment is empty or is not an ASCII identifier, so
/// the caller can report [`LexError::InvalidDirectivePath`](crate::error::LexError::InvalidDirectivePath).
pub(crate) fn split_directive_path(text: &str) -> Option<Vec<&str>> {
    if text.is_empty() {
        return None;
    }
    let mut path = Vec::new();
    for segment in text.split('.') {
        if !is_identifier(segment) {
            return None;
        }
        path.push(segment);
    }
    Some(path)
}

/// `name` / `name_1` — an ASCII identifier, which is what directive path
/// segments are restricted to.
fn is_identifier(segment: &str) -> bool {
    let mut chars = segment.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Byte index of the first `)` in `text` that is not inside a quoted string,
/// or `None` if there is none before the end of `text`.
///
/// `'…'` and `"…"` both hide a paren; `''` inside a single-quoted string is one
/// escaped quote and does not close it.
pub(crate) fn find_close_paren(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            q @ (b'\'' | b'"') => match quote {
                // A doubled quote inside a string is an escaped quote.
                Some(open) if q == open && bytes.get(i + 1) == Some(&q) => i += 2,
                Some(open) if q == open => {
                    quote = None;
                    i += 1;
                }
                None => {
                    quote = Some(q);
                    i += 1;
                }
                // A `"` inside `'…'` (or vice versa) is literal.
                Some(_) => i += 1,
            },
            b')' if quote.is_none() => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// The text of `line` up to, but excluding, the next `\n`.
pub(crate) fn first_line(text: &str) -> &str {
    match text.find('\n') {
        Some(i) => &text[..i],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_display_uses_file_line_col() {
        let span = Span::new("db/src/migrations/one.sql", 3, 7);
        assert_eq!(span.to_string(), "db/src/migrations/one.sql:3:7");
    }

    #[test]
    fn span_display_falls_back_to_input_for_in_memory_sources() {
        assert_eq!(Span::in_memory(1, 1).to_string(), "<input>:1:1");
    }

    #[test]
    fn directive_path_splits_on_dots() {
        assert_eq!(split_directive_path("up.begin"), Some(vec!["up", "begin"]));
        assert_eq!(split_directive_path("skipTx"), Some(vec!["skipTx"]));
    }

    #[test]
    fn directive_path_rejects_empty_and_non_identifier_segments() {
        assert_eq!(split_directive_path(""), None);
        assert_eq!(split_directive_path("up..end"), None);
        assert_eq!(split_directive_path("up."), None);
        assert_eq!(split_directive_path("1up"), None);
        // The `Open questions` item "directive mid-line": a space inside the
        // path is not an identifier, so it is an invalid path rather than a
        // key that happens to contain a space.
        assert_eq!(split_directive_path("up begin"), None);
        assert_eq!(split_directive_path("up-end"), None);
    }

    #[test]
    fn close_paren_ignores_parens_inside_quotes() {
        assert_eq!(find_close_paren("x)"), Some(1));
        // The `)` at index 2 is inside `'…'`, so the close is the one after it.
        assert_eq!(find_close_paren("'a)b')"), Some(5));
        // A `"` inside `'…'` is literal, and vice versa.
        assert_eq!(find_close_paren("'a\"b)'"), None);
        assert_eq!(find_close_paren("\"a)b\""), None);
        assert_eq!(find_close_paren("a"), None);
    }

    #[test]
    fn close_paren_survives_a_doubled_quote() {
        // `'it''s)'`: the doubled quote is escaped, so the string is still open
        // at the `)` and there is no close paren outside a quote.
        assert_eq!(find_close_paren("'it''s)'"), None);
        assert_eq!(find_close_paren("'it''s')"), Some(7));
    }
}
