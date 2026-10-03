//! The character-level scanner: one pass that splits SQL into statements and
//! lifts `--migrate:` comment lines out as directives.
//!
//! Both jobs have to happen in the same pass. Splitting on `;` first and finding
//! directives second breaks on strings and dollar-quotes containing `;`; finding
//! directive lines first and splitting second breaks when a `--migrate:`-shaped
//! line sits inside a `plpgsql` function body. So [`Lexer`] walks the text once,
//! carrying a [`LexState`] that says whether the characters being read are code,
//! a string, a quoted identifier, a dollar-quoted body, or a comment.
//!
//! Only `LexState::Normal` treats `;` as a statement terminator; every other
//! state exists to make `;` (and `--`, and `/*`) inert while inside it.

mod token;

pub use token::{LexToken, RawDirective, Span};

use std::path::{Path, PathBuf};

use crate::error::LexError;
use crate::model::Statement;

use token::{find_close_paren, first_line, split_directive_path};

/// What has to follow `--` for a comment line to be a directive. Byte-exact and
/// case-sensitive: `-- migrate:` and `--MIGRATE:` are ordinary comments.
pub const DIRECTIVE_MARKER: &str = "migrate:";

/// The line-comment opener that a directive must be written with.
pub const LINE_COMMENT_OPEN: &str = "--";

/// Where a file's last character leaves the cursor — the position reported for
/// errors raised at end of input.
pub fn end_span(source: &str, file: &Path) -> Span {
    let mut line = 1;
    let mut col = 1;
    for c in source.chars() {
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    Span::new(file, line, col)
}

/// Where the scanner currently is.
///
/// Exposed for tests and diagnostics: a failure at end of input is identified by
/// the state the scanner was in, and `BlockComment` carries how deep the nesting
/// got (PostgreSQL nests `/* /* */ */`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexState {
    /// Reading SQL. Only here does `;` end a statement.
    Normal,
    /// Inside a `-- …` comment, up to the next newline.
    LineComment,
    /// Inside a `/* … */` comment, `depth` levels deep.
    BlockComment {
        /// Nesting depth; closes when it returns to zero.
        depth: u32,
    },
    /// Inside a `'…'` string literal. `''` is one escaped quote.
    SingleQuoted,
    /// Inside a `"…"` quoted identifier. `""` is one escaped quote.
    DoubleQuoted,
    /// Inside a dollar-quoted body opened by `tag` (`$$`, `$fn$`, …), which only
    /// ends at that same tag.
    DollarQuoted {
        /// The opening tag, including both `$`.
        tag: String,
    },
}

type ScanResult<'a> = Result<Option<LexToken<'a>>, LexError>;

/// Splits migration source into [`LexToken`]s.
///
/// The lexer is deliberately directive-agnostic: it knows a `--migrate:` line is
/// not SQL text and nothing else. It never validates that `up.begin` is a
/// directive that exists — that is
/// [`DirectiveRegistry`](crate::directive::registry::DirectiveRegistry)'s job —
/// and it emits [`Statement`]s with empty `options`, because what applies to a
/// statement is a directive semantic.
///
/// Errors end the stream: the iterator yields one `Err` and then `None`.
#[derive(Debug)]
pub struct Lexer<'a> {
    source: &'a str,
    file: PathBuf,
    /// Byte offset into `source`.
    pos: usize,
    line: usize,
    col: usize,
    state: LexState,
    /// Where the current quoted region or block comment opened, so an
    /// unterminated one can be reported at its start rather than at EOF.
    region_start: Option<Span>,
    /// Text of the statement being accumulated. Comments never enter it.
    stmt: String,
    /// Span of the current statement's first non-whitespace character.
    stmt_start: Option<Span>,
    done: bool,
}

impl<'a> Lexer<'a> {
    /// A lexer over `source`, attributed to `file` in spans.
    pub fn new(source: &'a str, file: impl Into<PathBuf>) -> Self {
        Self {
            source,
            file: file.into(),
            pos: 0,
            line: 1,
            col: 1,
            state: LexState::Normal,
            region_start: None,
            stmt: String::new(),
            stmt_start: None,
            done: false,
        }
    }

    /// The state the scanner is in. Only meaningful between `next()` calls.
    pub fn state(&self) -> &LexState {
        &self.state
    }

    /// The position of the next character to read.
    pub fn here(&self) -> Span {
        Span::new(self.file.clone(), self.line, self.col)
    }

    fn peek(&self) -> Option<char> {
        let source: &'a str = self.source;
        source[self.pos..].chars().next()
    }

    fn rest(&self) -> &'a str {
        let source: &'a str = self.source;
        &source[self.pos..]
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    /// Record the start of a statement at the current position if this is its
    /// first non-whitespace character.
    fn mark_statement_start(&mut self, c: char) {
        if self.stmt_start.is_none() && !c.is_whitespace() {
            self.stmt_start = Some(self.here());
        }
    }

    /// Copy the current character into the statement text and consume it.
    fn push_code(&mut self, c: char) {
        self.mark_statement_start(c);
        self.stmt.push(c);
        self.bump();
    }

    /// Enter a quoted region or comment, remembering where it opened *before*
    /// its opener is consumed, so an unterminated one reports the opener.
    fn enter_region(&mut self, at: Span, state: LexState) {
        self.region_start = Some(at);
        self.state = state;
    }

    fn leave_region(&mut self) {
        self.region_start = None;
        self.state = LexState::Normal;
    }

    /// End the current statement, if there is one.
    fn flush_statement(&mut self) -> Option<LexToken<'a>> {
        let start = self.stmt_start.take();
        let sql = self.stmt.trim().to_string();
        self.stmt.clear();
        let span = start?;
        if sql.is_empty() {
            return None;
        }
        Some(LexToken::Statement(Statement::new(sql, span)))
    }

    fn scan_step(&mut self) -> ScanResult<'a> {
        let Some(c) = self.peek() else {
            return self.finish_at_eof();
        };
        match self.state {
            LexState::Normal => self.scan_normal(c),
            LexState::SingleQuoted => self.scan_single_quoted(c),
            LexState::DoubleQuoted => self.scan_double_quoted(c),
            LexState::DollarQuoted { .. } => self.scan_dollar_quoted(c),
            LexState::BlockComment { .. } => self.scan_block_comment(c),
            // Transient: a comment run is always finished before `scan_step`
            // returns. Kept as a resume path so a mid-comment exit can never
            // strand the scanner.
            LexState::LineComment => self.scan_line_comment(),
        }
    }

    fn scan_normal(&mut self, c: char) -> ScanResult<'a> {
        match c {
            ';' => {
                self.bump();
                Ok(self.flush_statement())
            }
            '\'' => {
                let opened = self.here();
                self.push_code(c);
                self.enter_region(opened, LexState::SingleQuoted);
                Ok(None)
            }
            '"' => {
                let opened = self.here();
                self.push_code(c);
                self.enter_region(opened, LexState::DoubleQuoted);
                Ok(None)
            }
            '$' => self.scan_dollar_open(),
            '-' if self.rest().starts_with(LINE_COMMENT_OPEN) => self.enter_line_comment(),
            '/' if self.rest().starts_with("/*") => {
                let opened = self.here();
                self.bump();
                self.bump();
                self.enter_region(opened, LexState::BlockComment { depth: 1 });
                Ok(None)
            }
            _ => {
                self.push_code(c);
                Ok(None)
            }
        }
    }

    /// A `'…'` string body. `''` is one escaped quote, so it keeps the string
    /// open; it is the only escape honoured.
    fn scan_single_quoted(&mut self, c: char) -> ScanResult<'a> {
        // DECISION: `E'…'` strings with backslash escapes are lexed exactly like
        // `'…'`. `standard_conforming_strings` is on by default in every
        // supported PostgreSQL, where a backslash before `;'` is literal; a
        // database that has turned it off is a known gap, flagged here rather
        // than silently mis-lexed.
        self.push_code(c);
        if c == '\'' {
            if self.peek() == Some('\'') {
                self.push_code('\'');
            } else {
                self.leave_region();
            }
        }
        Ok(None)
    }

    /// A `"…"` quoted identifier. Doubled quotes escape, as in string literals.
    fn scan_double_quoted(&mut self, c: char) -> ScanResult<'a> {
        self.push_code(c);
        if c == '"' {
            if self.peek() == Some('"') {
                self.push_code('"');
            } else {
                self.leave_region();
            }
        }
        Ok(None)
    }

    /// A `$…$` in `Normal` state: either the start of a dollar-quoted body or a
    /// literal `$` (a positional parameter such as `$1`).
    fn scan_dollar_open(&mut self) -> ScanResult<'a> {
        match dollar_tag_len(self.rest()) {
            Some(len) => {
                let source: &'a str = self.source;
                let tag = &source[self.pos..self.pos + len];
                self.mark_statement_start('$');
                let opened = self.here();
                self.stmt.push_str(tag);
                for _ in 0..tag.chars().count() {
                    self.bump();
                }
                self.enter_region(
                    opened,
                    LexState::DollarQuoted {
                        tag: tag.to_string(),
                    },
                );
                Ok(None)
            }
            None => {
                self.push_code('$');
                Ok(None)
            }
        }
    }

    /// A dollar-quoted body. Only the *same* tag closes it — a `$fn$` body runs
    /// past `$$` and `$other$`.
    fn scan_dollar_quoted(&mut self, c: char) -> ScanResult<'a> {
        let closes = c == '$'
            && match &self.state {
                LexState::DollarQuoted { tag } => self.rest().starts_with(tag.as_str()),
                _ => false,
            };
        if closes {
            let tag = match &self.state {
                LexState::DollarQuoted { tag } => tag.clone(),
                _ => String::new(),
            };
            self.stmt.push_str(&tag);
            for _ in 0..tag.chars().count() {
                self.bump();
            }
            self.leave_region();
            return Ok(None);
        }
        self.push_code(c);
        Ok(None)
    }

    /// A `/* … */` body, tracking PostgreSQL's nesting. Its text is discarded:
    /// a comment never becomes part of `Statement::sql`.
    fn scan_block_comment(&mut self, _c: char) -> ScanResult<'a> {
        // DECISION: `--migrate:` inside a block comment is never a directive.
        // Directives are recognised only from a `--` line comment entered in
        // `Normal` state, so a block comment hides one completely.
        let depth = match self.state {
            LexState::BlockComment { depth } => depth,
            _ => 0,
        };
        if self.rest().starts_with("/*") {
            self.bump();
            self.bump();
            self.state = LexState::BlockComment { depth: depth + 1 };
            return Ok(None);
        }
        if self.rest().starts_with("*/") {
            self.bump();
            self.bump();
            if depth <= 1 {
                // One space stands in for the comment so the tokens on either
                // side of it stay separate.
                self.stmt.push(' ');
                self.leave_region();
            } else {
                self.state = LexState::BlockComment { depth: depth - 1 };
            }
            return Ok(None);
        }
        self.bump();
        Ok(None)
    }

    /// A `-- …` comment. Decides directive-or-not here, at the moment `--` is
    /// seen in `Normal` state, because that is the only point where the choice
    /// is unambiguous.
    //
    // DECISION: the plan's open question about a directive sharing a line with
    // SQL (`SELECT 1; --migrate:skipTx`) is resolved in favour of accepting it.
    // Position in the token stream is what carries meaning — `skipTx` applies to
    // the statement that follows, wherever on the line the comment sits — and
    // rejecting it would have the lexer declare a comment illegal rather than a
    // directive meaningless. A `skipTx` that ends up with no statement after it
    // is still an `OrphanedStatementDirective` in `ParseContext`, which is where
    // that mistake actually hurts.
    fn enter_line_comment(&mut self) -> ScanResult<'a> {
        let source: &'a str = self.source;
        let start = self.here();
        self.enter_region(start.clone(), LexState::LineComment);
        // The dashes themselves are comment text; they never reach `Statement`.
        self.bump();
        self.bump();

        if !self.rest().starts_with(DIRECTIVE_MARKER) {
            // Rule 1, the "ignore" half: `-- migrate:up.begin` (a space, or any
            // other text after `--`) is an ordinary comment. Discarded, no
            // token, no warning, and it does not appear in statement text.
            return self.scan_line_comment();
        }

        // Rule 1, the "error" half: the exact `migrate:` prefix matched, so this
        // line *is* a directive and a malformed body cannot degrade to a
        // comment.
        let body = first_line(&source[self.pos..]);
        let body = &body[DIRECTIVE_MARKER.len()..];
        if body.is_empty() {
            return Err(LexError::EmptyDirective { span: start });
        }
        if body.starts_with([' ', '\t']) {
            return Err(LexError::WhitespaceInDirective { span: start });
        }

        let (path_text, args) = split_directive_body(body, &start)?;
        let path = split_directive_path(path_text.trim_end()).ok_or_else(|| {
            LexError::InvalidDirectivePath {
                path: path_text.trim_end().to_string(),
                span: start.clone(),
            }
        })?;

        // The rest of the line belongs to the directive, never to SQL.
        self.scan_line_comment()?;
        Ok(Some(LexToken::Directive(RawDirective {
            path,
            args,
            span: start,
        })))
    }

    /// Consume to end of line. A comment contributes nothing to statement text
    /// beyond a single separating space: dropping it entirely would weld
    /// `SELECT /* c */ 1` into `SELECT1`.
    fn scan_line_comment(&mut self) -> ScanResult<'a> {
        self.state = LexState::LineComment;
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.bump();
        }
        self.stmt.push(' ');
        self.leave_region();
        Ok(None)
    }

    /// End of input: an unterminated region is an error here rather than a
    /// silently truncated statement.
    fn finish_at_eof(&mut self) -> ScanResult<'a> {
        let region = self.region_start.clone().unwrap_or_else(|| self.here());
        let error = match self.state {
            LexState::SingleQuoted => Some(LexError::UnterminatedString { span: region }),
            LexState::DoubleQuoted => Some(LexError::UnterminatedQuotedIdentifier { span: region }),
            LexState::DollarQuoted { ref tag } => Some(LexError::UnterminatedDollarQuote {
                tag: tag.clone(),
                span: region,
            }),
            LexState::BlockComment { .. } => {
                Some(LexError::UnterminatedBlockComment { span: region })
            }
            LexState::Normal | LexState::LineComment => None,
        };
        self.done = true;
        if let Some(error) = error {
            return Err(error);
        }
        Ok(self.flush_statement())
    }
}

impl<'a> Iterator for Lexer<'a> {
    type Item = Result<LexToken<'a>, LexError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        loop {
            match self.scan_step() {
                Ok(Some(token)) => return Some(Ok(token)),
                Ok(None) => {
                    if self.done {
                        return None;
                    }
                }
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

/// The byte length of an opening `$…$` tag at the start of `text`, or `None` if
/// `text` does not begin with one.
///
/// PostgreSQL tags are empty (`$$`) or `[A-Za-z_][A-Za-z0-9_]*`. A `$` followed
/// by a digit is a positional parameter, not a tag, which is what keeps `$1`
/// from opening a phantom quoted body that swallows the rest of the file.
fn dollar_tag_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'$') {
        return None;
    }
    if bytes.get(1).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'$' => return Some(i + 1),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' => i += 1,
            _ => return None,
        }
    }
    None
}

/// Split a directive body into its path text and the raw text inside the
/// parentheses.
///
/// `body` is the text after `--migrate:` up to end of line and is known not to
/// start with whitespace.
fn split_directive_body<'a>(
    body: &'a str,
    span: &Span,
) -> Result<(&'a str, Option<&'a str>), LexError> {
    let Some(open) = body.find('(') else {
        return Ok((body, None));
    };
    let path_text = &body[..open];
    let inside = &body[open + 1..];
    let Some(close) = find_close_paren(inside) else {
        return Err(LexError::UnterminatedDirectiveArgs { span: span.clone() });
    };
    let trailing = inside[close + 1..].trim();
    if !trailing.is_empty() {
        return Err(LexError::UnexpectedDirectiveText {
            text: trailing.to_string(),
            span: span.clone(),
        });
    }
    Ok((path_text, Some(&inside[..close])))
}

#[cfg(test)]
mod tests {
    // Traceability to `db/src/migrations/PLAN.md`, "Verification → Lexer-level
    // tests" and "Things to get right that are easy to get wrong":
    //
    // | PLAN bullet | Test |
    // |---|---|
    // | `;` inside `'…'` / `"…"` / `$$…$$` / `$tag$…$tag$` / `--` / `/* */` / nested `/* */` does not split | `semicolon_in_a_single_quoted_string_does_not_split`, `semicolon_in_a_double_quoted_identifier_does_not_split`, `semicolon_in_an_untagged_dollar_quote_does_not_split`, `semicolon_in_a_tagged_dollar_quote_does_not_split`, `semicolon_in_a_line_comment_does_not_split`, `semicolon_in_a_block_comment_does_not_split`, `semicolon_in_a_nested_block_comment_does_not_split` |
    // | `''` is one escaped quote, not a terminator | `doubled_single_quote_is_one_escaped_quote_not_a_terminator` (and the `""` twin) |
    // | `--migrate:` inside a dollar-quoted body is literal text | `directive_inside_a_dollar_quoted_body_is_literal_text` |
    // | `-- migrate:up.begin` is an ordinary comment, emits no directive token and leaves no text | `space_after_dashes_makes_an_ordinary_comment_not_a_directive` |
    // | `--migrate: up.begin` is `LexError::WhitespaceInDirective` | `space_after_the_colon_is_a_whitespace_in_directive_error`, `tab_after_the_colon_is_the_same_error`, `whitespace_only_body_is_a_whitespace_error` |
    // | `migrate:left` with no `--` is not recognised at all | `text_without_a_line_comment_is_never_a_directive` |
    // | Unterminated string / dollar quote / block comment give distinct errors | `unterminated_single_quoted_string_is_its_own_error`, `unterminated_quoted_identifier_is_its_own_error`, `unterminated_dollar_quote_is_its_own_error_and_names_the_tag`, `unterminated_block_comment_is_its_own_error` |
    // | Prefix match is byte-exact and case-sensitive | `directive_recognition_is_case_sensitive` |
    // | Dollar-quote tags match exactly on close | `dollar_quote_tags_match_exactly_on_close`, `a_positional_parameter_dollar_does_not_open_a_quoted_body` |
    use super::*;

    /// Lex everything, expecting success.
    fn tokens(source: &str) -> Vec<LexToken<'_>> {
        Lexer::new(source, "test.sql")
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|e| panic!("unexpected lex error for {source:?}: {e}"))
    }

    /// The SQL text of every statement token.
    fn sqls(source: &str) -> Vec<String> {
        tokens(source)
            .into_iter()
            .filter_map(|t| match t {
                LexToken::Statement(s) => Some(s.sql),
                LexToken::Directive(_) => None,
            })
            .collect()
    }

    fn directive_keys(source: &str) -> Vec<String> {
        tokens(source)
            .into_iter()
            .filter_map(|t| match t {
                LexToken::Directive(d) => Some(d.key()),
                LexToken::Statement(_) => None,
            })
            .collect()
    }

    fn expect_err(source: &str) -> LexError {
        Lexer::new(source, "test.sql")
            .collect::<Result<Vec<_>, _>>()
            .expect_err("expected a lex error")
    }

    /// Whitespace-normalised text, for comparing statement text where a
    /// removed comment leaves a run of spaces behind.
    fn collapse(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    // ==== statement splitting ==============================================

    #[test]
    fn splits_on_top_level_semicolons() {
        assert_eq!(
            sqls("SELECT 1; SELECT 2;"),
            vec!["SELECT 1".to_string(), "SELECT 2".to_string()]
        );
    }

    #[test]
    fn emits_a_statement_not_terminated_by_a_semicolon() {
        assert_eq!(sqls("SELECT 1"), vec!["SELECT 1".to_string()]);
    }

    #[test]
    fn semicolons_without_statements_produce_nothing() {
        assert!(sqls(";;;").is_empty());
        assert!(sqls("").is_empty());
        assert!(sqls("   \n\t ").is_empty());
    }

    // "easy to get wrong": `;` inside these must not split a statement.
    #[test]
    fn semicolon_in_a_single_quoted_string_does_not_split() {
        assert_eq!(
            sqls("INSERT INTO t VALUES ('a; b');"),
            vec!["INSERT INTO t VALUES ('a; b')".to_string()]
        );
    }

    #[test]
    fn semicolon_in_a_double_quoted_identifier_does_not_split() {
        assert_eq!(
            sqls(r#"SELECT "odd;name" FROM t;"#),
            vec![r#"SELECT "odd;name" FROM t"#.to_string()]
        );
    }

    #[test]
    fn semicolon_in_an_untagged_dollar_quote_does_not_split() {
        assert_eq!(
            sqls("COMMENT ON TABLE t IS $$who\nsigned it; nobody$$;"),
            vec!["COMMENT ON TABLE t IS $$who\nsigned it; nobody$$".to_string()]
        );
    }

    #[test]
    fn semicolon_in_a_tagged_dollar_quote_does_not_split() {
        let source = "CREATE FUNCTION f() RETURNS int LANGUAGE plpgsql AS $fn$\n\
                      BEGIN\n    RETURN 1;\nEND;\n$fn$;\nSELECT 2;";
        let statements = sqls(source);
        assert_eq!(statements.len(), 2, "{statements:#?}");
        assert!(statements[0].contains("RETURN 1;"), "{:?}", statements[0]);
        assert_eq!(statements[1], "SELECT 2");
    }

    #[test]
    fn semicolon_in_a_line_comment_does_not_split() {
        assert_eq!(
            sqls("-- a comment; with a semicolon\nSELECT 1;"),
            vec!["SELECT 1".to_string()]
        );
    }

    #[test]
    fn semicolon_inside_a_statement_line_comment_does_not_split() {
        // The real shape that broke the old splitter: a column-level comment
        // with a `;` between the parentheses of a CREATE TABLE.
        let source = "CREATE TABLE t (\n    id INT,\n    -- comment; here\n    name TEXT\n);\n";
        let statements = sqls(source);
        assert_eq!(statements.len(), 1, "{statements:#?}");
        assert!(statements[0].contains("name TEXT"));
    }

    #[test]
    fn semicolon_in_a_block_comment_does_not_split() {
        assert_eq!(
            sqls("/* this; is; a; comment */ SELECT 1;"),
            vec!["SELECT 1".to_string()]
        );
    }

    #[test]
    fn semicolon_in_a_nested_block_comment_does_not_split() {
        let source = "/* outer; /* inner; deeper */ still; */ SELECT 1;";
        assert_eq!(sqls(source), vec!["SELECT 1".to_string()]);
        // And the outer comment really did nest: the first inner `*/` only
        // closed the inner level.
        let tokens = tokens("/* a; /* b; */ c; */\nSELECT 1; SELECT 2;");
        assert_eq!(tokens.len(), 2);
    }

    #[test]
    fn doubled_single_quote_is_one_escaped_quote_not_a_terminator() {
        // `''` inside the string: the string is still open at the `;`, so the
        // whole thing is one statement.
        assert_eq!(
            sqls("INSERT INTO t VALUES ('it''s; here'); SELECT 2;"),
            vec![
                "INSERT INTO t VALUES ('it''s; here')".to_string(),
                "SELECT 2".to_string(),
            ]
        );
    }

    #[test]
    fn doubled_double_quote_is_one_escaped_quote_not_a_terminator() {
        assert_eq!(
            sqls(r#"SELECT "it""s; name" FROM t; SELECT 2;"#),
            vec![
                r#"SELECT "it""s; name" FROM t"#.to_string(),
                "SELECT 2".to_string(),
            ]
        );
    }

    #[test]
    fn an_e_string_follows_standard_conforming_rules() {
        // DECISION, tested: `E'a\''` is lexed with standard-conforming rules, so
        // the backslash does not escape the quote and the string ends at the
        // second `'`. A server running with `standard_conforming_strings = off`
        // would see one long unterminated string here instead; v1 does not detect
        // that configuration.
        assert_eq!(
            sqls(r"INSERT INTO t VALUES (E'a\'); SELECT 2;"),
            vec![
                r"INSERT INTO t VALUES (E'a\')".to_string(),
                "SELECT 2".to_string(),
            ]
        );
    }

    #[test]
    fn dollar_quote_tags_match_exactly_on_close() {
        // "easy to get wrong": `$foo$` closes only on another literal `$foo$`,
        // not on `$$` or `$bar$`.
        let source = "CREATE FUNCTION f() AS $foo$\n\
                      SELECT $$inner; semicolon$$;\n\
                      SELECT $bar$other; body$bar$;\n\
                      RETURN 1;\n$foo$;\nSELECT 2;";
        let statements = sqls(source);
        assert_eq!(statements.len(), 2, "{statements:#?}");
        assert!(statements[0].contains("RETURN 1;"), "{:?}", statements[0]);
        assert!(statements[0].trim_end().ends_with("$foo$"));
        assert_eq!(statements[1], "SELECT 2");
    }

    #[test]
    fn a_positional_parameter_dollar_does_not_open_a_quoted_body() {
        // `$1` looks like a tag start and is not one; treating it as one would
        // glue every following statement into a phantom body.
        assert_eq!(
            sqls("INSERT INTO t (a, b) VALUES ($1, $2);\nSELECT a FROM t;"),
            vec![
                "INSERT INTO t (a, b) VALUES ($1, $2)".to_string(),
                "SELECT a FROM t".to_string(),
            ]
        );
    }

    #[test]
    fn statement_text_is_trimmed_and_excludes_the_semicolon() {
        let statements = sqls("\n\n   SELECT   1   ;\n");
        assert_eq!(statements, vec!["SELECT   1".to_string()]);
    }

    // ==== comments and directives ==========================================

    #[test]
    fn ordinary_comments_are_discarded_from_statement_text() {
        let statements = sqls("SELECT 1 -- trailing note\n; SELECT /* inline */ 2;");
        assert_eq!(statements.len(), 2, "{statements:#?}");
        // A dropped comment leaves one space in its place, so words on either
        // side never weld together; the run of spaces that can leave behind is
        // what that costs, and PostgreSQL does not care.
        assert_eq!(collapse(&statements[0]), "SELECT 1");
        assert_eq!(collapse(&statements[1]), "SELECT 2");
        assert!(
            !statements
                .iter()
                .any(|s| s.contains("note") || s.contains("inline")),
            "{statements:#?}"
        );
    }

    #[test]
    fn a_comment_between_two_words_keeps_them_separate() {
        // Dropping a comment with nothing would turn `SELECT/* c */1` into the
        // single word `SELECT1`.
        assert_eq!(sqls("SELECT/* c */1;"), vec!["SELECT 1".to_string()]);
        assert_eq!(collapse(&sqls("SELECT-- c\n1;")[0]), "SELECT 1");
    }

    // Rule 1, the "ignore" half.
    #[test]
    fn space_after_dashes_makes_an_ordinary_comment_not_a_directive() {
        let lexed = tokens("-- migrate:up.begin\nSELECT 1;");
        assert_eq!(
            lexed.len(),
            1,
            "no Directive token may be emitted: {lexed:#?}"
        );
        match &lexed[0] {
            LexToken::Statement(s) => {
                assert_eq!(s.sql, "SELECT 1");
                assert!(
                    !s.sql.contains("migrate"),
                    "the comment must not leak into statement text: {:?}",
                    s.sql
                );
            }
            other => panic!("expected a statement, got {other:?}"),
        }
    }

    #[test]
    fn directive_recognition_is_case_sensitive() {
        assert!(directive_keys("--MIGRATE:up.begin\nSELECT 1;").is_empty());
        assert_eq!(
            directive_keys("--migrate:up.begin\nSELECT 1;"),
            vec!["up.begin"]
        );
    }

    // Rule 1, the "error" half: the prefix matched, so whitespace is fatal.
    #[test]
    fn space_after_the_colon_is_a_whitespace_in_directive_error() {
        let error = expect_err("--migrate: up.begin\nSELECT 1;");
        assert!(
            matches!(error, LexError::WhitespaceInDirective { .. }),
            "got {error:?}"
        );
        assert_eq!(error.span(), &Span::new("test.sql", 1, 1));
    }

    #[test]
    fn tab_after_the_colon_is_the_same_error() {
        let error = expect_err("--migrate:\tskipTx\n");
        assert!(
            matches!(error, LexError::WhitespaceInDirective { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn text_without_a_line_comment_is_never_a_directive() {
        // `migrate:left` alone is just SQL/text content: recognition only
        // triggers from inside a line comment.
        let lexed = tokens("SELECT migrate:left FROM t;");
        assert_eq!(lexed.len(), 1);
        assert!(matches!(lexed[0], LexToken::Statement(_)));
    }

    #[test]
    fn directive_inside_a_dollar_quoted_body_is_literal_text() {
        let source = "CREATE FUNCTION f() AS $fn$\nBEGIN\n\
                      --migrate:up.begin\n\
                      RETURN 1;\nEND;\n$fn$;";
        let lexed = tokens(source);
        assert_eq!(
            lexed.len(),
            1,
            "the line must stay inside the body: {lexed:#?}"
        );
        match &lexed[0] {
            LexToken::Statement(s) => assert!(s.sql.contains("--migrate:up.begin")),
            other => panic!("expected one statement, got {other:?}"),
        }
    }

    #[test]
    fn directive_line_breaks_a_statement_without_joining_across_it() {
        // "Open questions": a directive occupying its own line between two
        // statements yields three tokens in stream order.
        let lexed = tokens("SELECT 1;\n--migrate:skipTx\nSELECT 2;");
        assert_eq!(lexed.len(), 3);
        assert!(matches!(lexed[1], LexToken::Directive(_)));
    }

    #[test]
    fn trailing_directive_on_a_statement_line_is_accepted() {
        // DECISION: the lexer is stream-oriented, so directive position is what
        // carries meaning and a directive may share a line with SQL. `skipTx`
        // here applies to the statement that follows on the next line.
        let lexed = tokens("SELECT 1; --migrate:skipTx\nSELECT 2;");
        assert_eq!(lexed.len(), 3, "{lexed:#?}");
        assert!(matches!(lexed[1], LexToken::Directive(_)));
    }

    #[test]
    fn directive_inside_a_block_comment_is_never_a_directive() {
        let lexed = tokens("/* --migrate:up.begin */ SELECT 1;");
        assert_eq!(lexed.len(), 1, "{lexed:#?}");
    }

    #[test]
    fn directives_are_lexed_with_their_path_and_args() {
        let lexed = tokens("--migrate:up.begin\n--migrate:skipTx\n--migrate:tool(name=\"x\", 2)\n");
        let keys: Vec<String> = lexed
            .iter()
            .filter_map(|t| match t {
                LexToken::Directive(d) => Some(d.key()),
                LexToken::Statement(_) => None,
            })
            .collect();
        assert_eq!(keys, vec!["up.begin", "skipTx", "tool"]);
        match &lexed[2] {
            LexToken::Directive(d) => {
                assert_eq!(d.path, vec!["tool"]);
                assert_eq!(d.args, Some("name=\"x\", 2"));
            }
            other => panic!("expected a directive, got {other:?}"),
        }
    }

    #[test]
    fn directive_without_arguments_has_none_rather_than_empty() {
        match &tokens("--migrate:skipTx\n")[0] {
            LexToken::Directive(d) => assert_eq!(d.args, None),
            other => panic!("expected a directive, got {other:?}"),
        }
    }

    #[test]
    fn empty_argument_list_is_present_but_empty() {
        match &tokens("--migrate:skipTx()\n")[0] {
            LexToken::Directive(d) => assert_eq!(d.args, Some("")),
            other => panic!("expected a directive, got {other:?}"),
        }
    }

    #[test]
    fn directive_span_points_at_the_dashes() {
        let lexed = tokens("SELECT 1;\n\n   --migrate:skipTx\n");
        match &lexed[1] {
            LexToken::Directive(d) => {
                assert_eq!(d.span.line, 3);
                assert_eq!(d.span.col, 4);
            }
            other => panic!("expected a directive, got {other:?}"),
        }
    }

    // ==== malformed directive bodies =======================================

    #[test]
    fn bare_prefix_with_no_path_is_an_error() {
        let error = expect_err("--migrate:\n");
        assert!(
            matches!(error, LexError::EmptyDirective { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn whitespace_only_body_is_a_whitespace_error() {
        let error = expect_err("--migrate:   \n");
        assert!(
            matches!(error, LexError::WhitespaceInDirective { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn invalid_directive_paths_are_errors() {
        for source in [
            "--migrate:up..end\n",
            "--migrate:up.\n",
            "--migrate:1up\n",
            "--migrate:up begin\n",
            "--migrate:up-end\n",
        ] {
            let error = expect_err(source);
            assert!(
                matches!(error, LexError::InvalidDirectivePath { .. }),
                "{source:?} gave {error:?}"
            );
        }
    }

    #[test]
    fn unclosed_directive_argument_list_is_an_error() {
        let error = expect_err("--migrate:tool(name\nSELECT 1;");
        assert!(
            matches!(error, LexError::UnterminatedDirectiveArgs { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn unterminated_quote_in_a_directive_argument_list_is_reported_there() {
        // The `)` is inside an open quote, so the argument list never closes.
        let error = expect_err("--migrate:tool(\"a)\nSELECT 1;");
        assert!(
            matches!(error, LexError::UnterminatedDirectiveArgs { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn text_after_a_closed_argument_list_is_an_error() {
        let error = expect_err("--migrate:tool(a) trailing\n");
        match error {
            LexError::UnexpectedDirectiveText { text, .. } => assert_eq!(text, "trailing"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn trailing_whitespace_after_the_argument_list_is_fine() {
        let lexed = tokens("--migrate:tool(a)   \nSELECT 1;");
        assert_eq!(lexed.len(), 2);
    }

    // ==== unterminated regions at EOF ======================================

    #[test]
    fn unterminated_single_quoted_string_is_its_own_error() {
        let error = expect_err("SELECT 'never closed");
        match error {
            LexError::UnterminatedString { span } => {
                assert_eq!((span.line, span.col), (1, 8));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn unterminated_quoted_identifier_is_its_own_error() {
        let error = expect_err("SELECT \"never closed");
        assert!(
            matches!(error, LexError::UnterminatedQuotedIdentifier { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn unterminated_dollar_quote_is_its_own_error_and_names_the_tag() {
        let error = expect_err("CREATE FUNCTION f() AS $fn$\nSELECT 1;\n");
        match error {
            LexError::UnterminatedDollarQuote { tag, span } => {
                assert_eq!(tag, "$fn$");
                assert_eq!((span.line, span.col), (1, 24));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn unterminated_block_comment_is_its_own_error() {
        let error = expect_err("/* nested /* deeper */ still open\nSELECT 1;");
        assert!(
            matches!(error, LexError::UnterminatedBlockComment { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn a_trailing_line_comment_at_eof_is_not_an_error() {
        assert_eq!(sqls("SELECT 1 -- note"), vec!["SELECT 1".to_string()]);
    }

    // ==== helpers ==========================================================

    #[test]
    fn dollar_tag_len_recognises_tags_and_rejects_parameters() {
        assert_eq!(dollar_tag_len("$$ rest"), Some(2));
        assert_eq!(dollar_tag_len("$fn$ body"), Some(4));
        assert_eq!(dollar_tag_len("$1"), None);
        assert_eq!(dollar_tag_len("$9x$"), None);
        assert_eq!(dollar_tag_len("$ unterminated"), None);
        assert_eq!(dollar_tag_len("$fn"), None);
        assert_eq!(dollar_tag_len(""), None);
    }

    #[test]
    fn end_span_counts_lines_and_columns() {
        assert_eq!(
            end_span("ab\ncd", Path::new("f.sql")).to_string(),
            "f.sql:2:3"
        );
        assert_eq!(end_span("", Path::new("f.sql")).to_string(), "f.sql:1:1");
    }

    #[test]
    fn multibyte_text_keeps_columns_in_characters_not_bytes() {
        let statements = sqls("SELECT 'héllo wörld';\n--migrate:skipTx\n");
        assert_eq!(statements.len(), 1);
        match &tokens("SELECT 'héllo wörld';\n--migrate:skipTx\n")[1] {
            LexToken::Directive(d) => assert_eq!(d.span.line, 2),
            other => panic!("got {other:?}"),
        }
    }

    // ==== no input can panic the lexer =====================================

    /// Deterministic pseudo-random source (xorshift), so a failure reproduces
    /// from its seed without a dependency on a rand crate.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    /// Characters chosen so that random draws land constantly on state
    /// transitions: every quote opener, the `;` terminator, both comment
    /// openers, the `*/` closer and the letters of the directive prefix.
    const HOSTILE: &[char] = &[
        '\'', '"', '$', ';', '-', '/', '*', 'm', 'i', 'g', 'r', 'a', 't', 'e', ':', ' ', '\n',
        '\t', '(', ')', '=', 'x', '\\', '0',
    ];

    #[test]
    fn arbitrary_input_never_panics_and_is_deterministic() {
        for seed in 0..4000u64 {
            let mut rng = Lcg(seed ^ 0x9E37_79B9_7F4A_7C15);
            let len = (rng.next() % 48) as usize;
            let source: String = (0..len)
                .map(|_| HOSTILE[(rng.next() % HOSTILE.len() as u64) as usize])
                .collect();

            let once = Lexer::new(&source, "fuzz.sql").collect::<Vec<_>>();
            let twice = Lexer::new(&source, "fuzz.sql").collect::<Vec<_>>();
            assert_eq!(
                once.iter().map(format_token).collect::<Vec<_>>(),
                twice.iter().map(format_token).collect::<Vec<_>>(),
                "lexing {source:?} (seed {seed}) is not deterministic"
            );

            for token in once {
                match token {
                    Ok(LexToken::Statement(statement)) => {
                        assert!(
                            !statement.sql.trim().is_empty()
                                && statement.sql == statement.sql.trim(),
                            "statement text is trimmed, never blank: {statement:?} in {source:?}"
                        );
                        assert!(
                            !statement.sql.ends_with(';'),
                            "the terminator is not part of statement text: {:?} in {source:?}",
                            statement.sql
                        );
                        assert!(
                            statement.span.line >= 1 && statement.span.col >= 1,
                            "every statement carries a position: {statement:?}"
                        );
                    }
                    Ok(LexToken::Directive(directive)) => {
                        assert!(
                            !directive.path.is_empty(),
                            "a directive token always has a key: {directive:?} in {source:?}"
                        );
                    }
                    Err(error) => {
                        let span = error.span();
                        assert!(
                            span.line >= 1 && span.col >= 1,
                            "every lex error carries a position: {error} for {source:?}"
                        );
                    }
                }
            }
        }
    }

    fn format_token(token: &Result<LexToken<'_>, LexError>) -> String {
        format!("{token:?}")
    }
}
