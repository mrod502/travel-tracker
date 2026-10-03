//! The parsed shape of a migration file.

use crate::lex::Span;

/// A modifier a directive attaches to the statement that follows it.
///
/// Options are a set, not a sequence: [`ParseContext::push_statement`]
/// deduplicates them, so the same directive written twice before one statement
/// has no additional effect.
///
/// [`ParseContext::push_statement`]: crate::parse::ParseContext::push_statement
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatementOption {
    /// Run this statement outside the surrounding transaction.
    ///
    /// Set by `--migrate:skipTx`. It exists for statements PostgreSQL refuses
    /// to run inside a transaction block — `CREATE INDEX CONCURRENTLY` above
    /// all — and it is the caller's job to honour it: the parser only records
    /// the intent.
    OptSkipTx,
}

/// One SQL statement, as the lexer carved it out of the file.
///
/// `sql` is the statement text with the terminating `;` removed and the
/// surrounding whitespace trimmed. Comments are dropped by the lexer, so they
/// never appear here; strings, quoted identifiers and dollar-quoted bodies are
/// preserved byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// Statement text, semicolon excluded.
    pub sql: String,
    /// Options attached by directives preceding the statement.
    ///
    /// Empty when the lexer produced the statement; the parser fills it in from
    /// the pending options in [`ParseContext::push_statement`].
    ///
    /// [`ParseContext::push_statement`]: crate::parse::ParseContext::push_statement
    pub options: Vec<StatementOption>,
    /// Where the statement starts, for error reporting.
    pub span: Span,
}

impl Statement {
    /// A statement with no options, as produced by the lexer.
    pub fn new(sql: impl Into<String>, span: Span) -> Self {
        Self {
            sql: sql.into(),
            options: Vec::new(),
            span,
        }
    }

    /// Does this statement carry `option`?
    pub fn has_option(&self, option: StatementOption) -> bool {
        self.options.contains(&option)
    }
}

/// A series of statements that run together — the `up` or the `down` of a file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatementGroup {
    /// The statements, in the order they appeared in the file.
    pub statements: Vec<Statement>,
}

impl StatementGroup {
    /// An empty group.
    pub fn new() -> Self {
        Self {
            statements: Vec::new(),
        }
    }

    /// A group over `statements`.
    pub fn from_statements(statements: Vec<Statement>) -> Self {
        Self { statements }
    }

    /// `true` when the group holds no statements.
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }

    /// How many statements the group holds.
    pub fn len(&self) -> usize {
        self.statements.len()
    }
}

/// A whole migration file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Migration {
    /// The forward direction: what moves the database state onward.
    ///
    /// **Required.** A file with no `--migrate:up*`/`--migrate:down*` directives
    /// at all is entirely `up`; so are any bare statements that appear before
    /// the first block directive. "Required" is enforced by
    /// [`MigrationParseError::EmptyUp`]: a migration whose `up` holds no
    /// statements is rejected rather than silently recorded as applied while
    /// changing nothing.
    ///
    /// [`MigrationParseError::EmptyUp`]: crate::error::MigrationParseError::EmptyUp
    pub up: StatementGroup,
    /// The inverse of `up`, from `--migrate:down.begin` / `--migrate:down.end`.
    ///
    /// **Optional.** `None` means the file declares no revert at all — distinct
    /// from an empty one, which is rejected as
    /// [`MigrationParseError::EmptyDown`](crate::error::MigrationParseError::EmptyDown).
    /// Nothing validates that `down` is the literal inverse of `up`; that is
    /// held by convention.
    pub down: Option<StatementGroup>,
}

impl Migration {
    /// A migration with an empty `up` and no `down`, as a starting point.
    pub fn new() -> Self {
        Self {
            up: StatementGroup::new(),
            down: None,
        }
    }

    /// Does this migration declare a revert?
    pub fn is_revertible(&self) -> bool {
        self.down.is_some()
    }

    /// Every statement in `up` that must run outside a transaction.
    ///
    /// A caller that runs `up` inside one transaction needs this to decide
    /// whether it can: PostgreSQL rejects `CREATE INDEX CONCURRENTLY` there.
    pub fn skip_tx_statements(&self) -> Vec<&Statement> {
        self.up
            .statements
            .iter()
            .filter(|s| s.has_option(StatementOption::OptSkipTx))
            .collect()
    }
}
