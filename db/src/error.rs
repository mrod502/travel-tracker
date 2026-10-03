//! Every failure the parser can produce, as typed variants carrying a [`Span`].
//!
//! The layering mirrors the pipeline: [`LexError`] from the scanner,
//! [`DirectiveParseError`] when a directive key is resolved and its arguments
//! read, [`DirectiveApplyError`] when a directive changes parser state, and
//! [`MigrationParseError`] as the single type a caller of
//! [`MigrationParser::parse`](crate::parse::MigrationParser::parse) handles.
//!
//! Nothing here is produced by `unwrap`/`panic`: every malformed input the
//! lexer, the argument parser, or a group transition can meet is a value.

use std::fmt;

use crate::directive::GroupFamily;
use crate::lex::Span;

/// A malformed directive argument list, as read by
/// [`parse_args`](crate::directive::args::parse_args).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgError {
    /// A `'...'` or `"..."` argument runs to the end of the text without
    /// closing.
    UnterminatedString {
        /// Where the opening quote is.
        span: Span,
    },
    /// A `(` or `)` appears where an argument character was expected — the
    /// parenthesised argument list is not balanced.
    UnbalancedParen {
        /// The offending character.
        found: char,
        /// Where it is.
        span: Span,
    },
    /// Two commas in a row, a trailing comma, or a bare `,`: an argument slot
    /// with nothing in it.
    EmptyArgument {
        /// Where the empty slot starts.
        span: Span,
    },
    /// `name=` with nothing after the `=`.
    MissingValue {
        /// The argument name left without a value.
        name: String,
        /// Where the name is.
        span: Span,
    },
    /// A `'` or `"` appears inside an otherwise bare (unquoted) argument, e.g.
    /// `a"b`.
    UnexpectedQuote {
        /// The offending quote character.
        found: char,
        /// Where it is.
        span: Span,
    },
    /// The same named argument was given twice.
    DuplicateName {
        /// The repeated name.
        name: String,
        /// Where the second occurrence is.
        span: Span,
    },
}

impl ArgError {
    /// The source location of the error, when it has one.
    pub fn span(&self) -> &Span {
        match self {
            Self::UnterminatedString { span }
            | Self::UnbalancedParen { span, .. }
            | Self::EmptyArgument { span }
            | Self::MissingValue { span, .. }
            | Self::UnexpectedQuote { span, .. }
            | Self::DuplicateName { span, .. } => span,
        }
    }
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnterminatedString { span } => {
                write!(f, "{span}: unterminated quoted argument")
            }
            Self::UnbalancedParen { found, span } => {
                write!(
                    f,
                    "{span}: unbalanced parentheses in directive arguments: unexpected `{found}`"
                )
            }
            Self::EmptyArgument { span } => {
                write!(f, "{span}: empty argument in directive argument list")
            }
            Self::MissingValue { name, span } => {
                write!(f, "{span}: argument `{name}` has no value")
            }
            Self::UnexpectedQuote { found, span } => write!(
                f,
                "{span}: unexpected `{found}` inside an unquoted directive argument"
            ),
            Self::DuplicateName { name, span } => {
                write!(
                    f,
                    "{span}: directive argument `{name}` given more than once"
                )
            }
        }
    }
}

impl std::error::Error for ArgError {}

/// A failure of the character-level scanner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexError {
    /// `--migrate:` matched but whitespace follows the colon, e.g.
    /// `--migrate: up.begin`.
    ///
    /// This is a hard error, unlike `-- migrate:up.begin`: once the exact
    /// `migrate:` prefix has matched, the line *is* a directive and a malformed
    /// body cannot fall back to being an ordinary comment.
    WhitespaceInDirective {
        /// Where the `--` that opens the directive is.
        span: Span,
    },
    /// The line is exactly `--migrate:` — a prefix with no directive path.
    EmptyDirective {
        /// Where the `--` is.
        span: Span,
    },
    /// A directive path that is not `name` or `name.name…` of ASCII
    /// identifiers — an empty segment (`up..end`), a segment starting with a
    /// digit, or a space inside the path (`up begin`).
    InvalidDirectivePath {
        /// The text that failed to parse as a path.
        path: String,
        /// Where the `--` is.
        span: Span,
    },
    /// `--migrate:key(` with no closing `)` before end of line, or a quoted
    /// argument left open so that the `)` is never seen outside a quote.
    UnterminatedDirectiveArgs {
        /// Where the `--` is.
        span: Span,
    },
    /// Text follows the closing `)` of a directive's argument list.
    UnexpectedDirectiveText {
        /// The trailing text.
        text: String,
        /// Where the `--` is.
        span: Span,
    },
    /// A `'…'` string literal that reaches end of input.
    UnterminatedString {
        /// Where the opening `'` is.
        span: Span,
    },
    /// A `"…"` quoted identifier that reaches end of input.
    UnterminatedQuotedIdentifier {
        /// Where the opening `"` is.
        span: Span,
    },
    /// A dollar-quoted body that reaches end of input without its closing tag.
    UnterminatedDollarQuote {
        /// The opening tag, e.g. `$$` or `$fn$`.
        tag: String,
        /// Where the opening tag is.
        span: Span,
    },
    /// A `/* … */` block comment (possibly nested) that reaches end of input.
    UnterminatedBlockComment {
        /// Where the opening `/*` is.
        span: Span,
    },
}

impl LexError {
    /// The source location of the error.
    pub fn span(&self) -> &Span {
        match self {
            Self::WhitespaceInDirective { span }
            | Self::EmptyDirective { span }
            | Self::InvalidDirectivePath { span, .. }
            | Self::UnterminatedDirectiveArgs { span }
            | Self::UnexpectedDirectiveText { span, .. }
            | Self::UnterminatedString { span }
            | Self::UnterminatedQuotedIdentifier { span }
            | Self::UnterminatedDollarQuote { span, .. }
            | Self::UnterminatedBlockComment { span } => span,
        }
    }
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WhitespaceInDirective { span } => write!(
                f,
                "{span}: whitespace between `--migrate:` and the directive path \
                 (write `--migrate:up.begin`; `-- migrate:up.begin` is just a comment)"
            ),
            Self::EmptyDirective { span } => {
                write!(
                    f,
                    "{span}: `--migrate:` has no directive path after the colon"
                )
            }
            Self::InvalidDirectivePath { path, span } => write!(
                f,
                "{span}: invalid directive path `{path}` \
                 (expected `name` or `name.name` of ASCII identifiers)"
            ),
            Self::UnterminatedDirectiveArgs { span } => {
                write!(
                    f,
                    "{span}: directive argument list is missing its closing `)`"
                )
            }
            Self::UnexpectedDirectiveText { text, span } => write!(
                f,
                "{span}: unexpected text `{text}` after the directive argument list"
            ),
            Self::UnterminatedString { span } => {
                write!(f, "{span}: unterminated string literal")
            }
            Self::UnterminatedQuotedIdentifier { span } => {
                write!(f, "{span}: unterminated quoted identifier")
            }
            Self::UnterminatedDollarQuote { tag, span } => write!(
                f,
                "{span}: unterminated dollar-quoted string: `{tag}` has no closing `{tag}`"
            ),
            Self::UnterminatedBlockComment { span } => {
                write!(f, "{span}: unterminated block comment")
            }
        }
    }
}

impl std::error::Error for LexError {}

/// A directive that could not be resolved or whose arguments are invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectiveParseError {
    /// No directive is registered under this key. A typo such as `up.bgin`
    /// lands here rather than being ignored.
    UnknownDirective {
        /// The dotted key that was not found, e.g. `up.bgin`.
        key: String,
        /// Where the `--` is.
        span: Span,
    },
    /// The argument list is malformed.
    Arguments(ArgError),
    /// The directive takes no arguments but was written with a parenthesised
    /// list, e.g. `--migrate:skipTx(scope="group")`.
    UnexpectedArguments {
        /// The directive key that was given arguments.
        key: String,
        /// Where the `--` is.
        span: Span,
    },
}

impl DirectiveParseError {
    /// The source location of the error.
    pub fn span(&self) -> &Span {
        match self {
            Self::UnknownDirective { span, .. } | Self::UnexpectedArguments { span, .. } => span,
            Self::Arguments(arg) => arg.span(),
        }
    }
}

impl fmt::Display for DirectiveParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDirective { key, span } => {
                write!(f, "{span}: unknown directive `{key}`")
            }
            Self::Arguments(arg) => write!(f, "{arg}"),
            Self::UnexpectedArguments { key, span } => {
                write!(f, "{span}: directive `{key}` does not take arguments")
            }
        }
    }
}

impl std::error::Error for DirectiveParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Arguments(arg) => Some(arg),
            _ => None,
        }
    }
}

impl From<ArgError> for DirectiveParseError {
    fn from(value: ArgError) -> Self {
        Self::Arguments(value)
    }
}

/// A well-formed directive whose effect on parser state is not allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectiveApplyError {
    /// `up.begin` while `up` is already open (`down.begin` inside `down`, and
    /// so on).
    DuplicateGroupOpen {
        /// The group opened twice.
        family: GroupFamily,
        /// Where the second `begin` is.
        span: Span,
    },
    /// A group that already closed is opened again — a file may define `up` at
    /// most once.
    GroupAlreadyDefined {
        /// The group defined twice.
        family: GroupFamily,
        /// Where the second `begin` is.
        span: Span,
    },
    /// `up.end` with no `up.begin` open.
    CloseWithoutOpen {
        /// The group that was closed.
        family: GroupFamily,
        /// Where the `end` is.
        span: Span,
    },
    /// `down.end` while `up` is the innermost open group.
    MismatchedClose {
        /// The group that is actually open.
        expected: GroupFamily,
        /// The group the directive tried to close.
        found: GroupFamily,
        /// Where the `end` is.
        span: Span,
    },
    /// End of file with a group still open.
    UnclosedGroup {
        /// The group left open.
        family: GroupFamily,
        /// Where its `begin` was.
        span: Span,
    },
    /// A statement-level directive (`--migrate:skipTx`) was followed by another
    /// directive or by end of file instead of a statement, so its option would
    /// have nowhere to go.
    OrphanedStatementDirective {
        /// Where the orphaned directive is.
        span: Span,
    },
}

impl DirectiveApplyError {
    /// The source location of the error.
    pub fn span(&self) -> &Span {
        match self {
            Self::DuplicateGroupOpen { span, .. }
            | Self::GroupAlreadyDefined { span, .. }
            | Self::CloseWithoutOpen { span, .. }
            | Self::MismatchedClose { span, .. }
            | Self::UnclosedGroup { span, .. }
            | Self::OrphanedStatementDirective { span } => span,
        }
    }
}

impl fmt::Display for DirectiveApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateGroupOpen { family, span } => write!(
                f,
                "{span}: `{family}.begin` appears while the {family} block is still open"
            ),
            Self::GroupAlreadyDefined { family, span } => write!(
                f,
                "{span}: the {family} block is already defined in this file; a group may be opened once"
            ),
            Self::CloseWithoutOpen { family, span } => {
                write!(
                    f,
                    "{span}: `{family}.end` appears with no open {family} block"
                )
            }
            Self::MismatchedClose {
                expected,
                found,
                span,
            } => write!(
                f,
                "{span}: `{found}.end` appears while the {expected} block is open"
            ),
            Self::UnclosedGroup { family, span } => {
                write!(
                    f,
                    "{span}: the {family} block is never closed before end of file"
                )
            }
            Self::OrphanedStatementDirective { span } => write!(
                f,
                "{span}: statement directive is not followed by a statement; \
                 its option would apply to nothing"
            ),
        }
    }
}

impl std::error::Error for DirectiveApplyError {}

/// A directive key registered twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationError {
    /// The key claimed by two directive types.
    pub key: String,
}

impl fmt::Display for RegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "directive `{}` is already registered; registration is rejected rather than overwritten",
            self.key
        )
    }
}

impl std::error::Error for RegistrationError {}

/// Anything that can go wrong while turning migration source into a
/// [`Migration`](crate::model::Migration).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationParseError {
    /// The scanner could not read the file.
    Lex(LexError),
    /// A directive was unknown or given invalid arguments.
    Directive(DirectiveParseError),
    /// A directive was valid but its structural effect is not allowed here.
    Apply(DirectiveApplyError),
    /// `up` holds no statements.
    ///
    /// A migration that records itself as applied while changing nothing is
    /// worse than one that fails, so this is refused whether it comes from a
    /// `down`-only file or from a file with no statements at all.
    EmptyUp {
        /// Where the missing `up` content should have been.
        span: Span,
    },
    /// `--migrate:down.begin`/`down.end` was written but holds no statements —
    /// a revert that would silently undo nothing. A file with no `down` at all
    /// is fine; that is `Migration::down == None`.
    EmptyDown {
        /// Where the `down.begin` is.
        span: Span,
    },
}

impl MigrationParseError {
    /// The source location of the error, if the failure has one.
    pub fn span(&self) -> Option<&Span> {
        match self {
            Self::Lex(e) => Some(e.span()),
            Self::Directive(e) => Some(e.span()),
            Self::Apply(e) => Some(e.span()),
            Self::EmptyUp { span } | Self::EmptyDown { span } => Some(span),
        }
    }
}

impl fmt::Display for MigrationParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lex(e) => write!(f, "{e}"),
            Self::Directive(e) => write!(f, "{e}"),
            Self::Apply(e) => write!(f, "{e}"),
            Self::EmptyUp { span } => write!(
                f,
                "{span}: migration has no `up` statements; \
                 a migration that changes nothing must not be recorded as applied"
            ),
            Self::EmptyDown { span } => write!(
                f,
                "{span}: the `down` block declares no statements; \
                 write no `down` block at all if the migration cannot be reverted"
            ),
        }
    }
}

impl std::error::Error for MigrationParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Lex(_) | Self::EmptyUp { .. } | Self::EmptyDown { .. } => None,
            Self::Directive(e) => Some(e),
            Self::Apply(e) => Some(e),
        }
    }
}

impl From<LexError> for MigrationParseError {
    fn from(value: LexError) -> Self {
        Self::Lex(value)
    }
}

impl From<DirectiveParseError> for MigrationParseError {
    fn from(value: DirectiveParseError) -> Self {
        Self::Directive(value)
    }
}

impl From<DirectiveApplyError> for MigrationParseError {
    fn from(value: DirectiveApplyError) -> Self {
        Self::Apply(value)
    }
}
