//! The mutable state threaded through a file: which group statements belong to,
//! and which options are waiting for the next statement.

use crate::directive::GroupFamily;
use crate::error::{DirectiveApplyError, MigrationParseError};
use crate::lex::Span;
use crate::model::{Migration, Statement, StatementGroup, StatementOption};

/// Parser state for one file.
///
/// The lexer produces a flat stream of statements and directives; `ParseContext`
/// is what turns that stream into a [`Migration`] by tracking
///
/// * which block is currently open (a stack, so a mismatched `end` is caught
///   rather than guessed at),
/// * which statement options are still waiting for a statement, and
/// * which groups have been declared, so a group cannot be defined twice.
///
/// Deviation from the plan's sketch: there is no separate `active` field. The
/// active family is the top of `open_blocks`, so the two can never disagree, and
/// statements accumulate per family rather than in one shared buffer — which is
/// what lets a `down` block nest inside an `up` block without stealing the
/// statements around it.
#[derive(Debug, Clone)]
pub struct ParseContext {
    /// Statements for `up`: those written bare plus those inside an `up` block.
    up: Vec<Statement>,
    /// Statements for `down`, if it was declared.
    down: Vec<Statement>,
    /// Open blocks, innermost last, each with the span of its `begin`.
    open_blocks: Vec<(GroupFamily, Span)>,
    /// Options queued by directives such as `skipTx`, drained by the next
    /// statement.
    pending_statement_options: Vec<StatementOption>,
    /// Where the directive that queued the first pending option is, so an
    /// orphaned option can name it.
    pending_span: Option<Span>,
    /// `up`/`down` were declared by a `begin` directive, even if empty.
    up_declared: bool,
    down_declared: bool,
    /// The first construct in the file, for the error raised when `up` ends up
    /// empty and there is nowhere better to point.
    first_construct: Option<Span>,
    /// Where `down.begin` is, so an empty `down` points at its declaration.
    down_begin_span: Option<Span>,
    /// The start of the file, as a last-resort span.
    file_start: Span,
}

impl ParseContext {
    /// A context for a file starting at `file_start`.
    pub fn new(file_start: Span) -> Self {
        Self {
            up: Vec::new(),
            down: Vec::new(),
            open_blocks: Vec::new(),
            pending_statement_options: Vec::new(),
            pending_span: None,
            up_declared: false,
            down_declared: false,
            first_construct: None,
            down_begin_span: None,
            file_start,
        }
    }

    /// The innermost open block, if any. Statements written outside every block
    /// belong to `up`.
    pub fn active(&self) -> Option<GroupFamily> {
        self.open_blocks.last().map(|(family, _)| *family)
    }

    /// The open blocks, outermost first, each with the span that opened it.
    pub fn open_blocks(&self) -> &[(GroupFamily, Span)] {
        &self.open_blocks
    }

    /// The options waiting for the next statement.
    pub fn pending_statement_options(&self) -> &[StatementOption] {
        &self.pending_statement_options
    }

    /// Statements collected for `up` so far.
    pub fn up_statements(&self) -> &[Statement] {
        &self.up
    }

    /// Statements collected for `down` so far.
    pub fn down_statements(&self) -> &[Statement] {
        &self.down
    }

    /// Queue `option` for the next statement.
    ///
    /// Idempotent: a second `skipTx` before the same statement is the same
    /// instruction, not a second one.
    pub fn push_pending_option(&mut self, option: StatementOption, span: &Span) {
        // DECISION: the plan asks what `skipTx` twice should mean; duplicate
        // options are collapsed rather than rejected, because options are a set
        // and an author repeating a comment (or moving one) has not asked for
        // anything different. An *orphaned* option is still an error.
        self.note_first(span);
        if !self.pending_statement_options.contains(&option) {
            self.pending_statement_options.push(option);
        }
        self.pending_span.get_or_insert_with(|| span.clone());
    }

    /// Attach any pending options to `statement` and file it under the open
    /// block (or `up`, when none is open).
    ///
    /// Pending options are cleared whether or not the statement ends up carrying
    /// any, so a `skipTx` can never leak onto a later, unrelated statement.
    pub fn push_statement(&mut self, mut statement: Statement) {
        self.note_first(&statement.span);
        statement
            .options
            .append(&mut self.pending_statement_options);
        self.pending_span = None;
        match self.active() {
            Some(GroupFamily::Up) | None => self.up.push(statement),
            Some(GroupFamily::Down) => self.down.push(statement),
        }
    }

    /// Open a group: statements that follow belong to it until the matching
    /// `end`.
    pub fn begin_group(
        &mut self,
        family: GroupFamily,
        span: &Span,
    ) -> Result<(), DirectiveApplyError> {
        self.reject_orphan(span)?;
        self.note_first(span);

        if self.open_blocks.iter().any(|(open, _)| *open == family) {
            return Err(DirectiveApplyError::DuplicateGroupOpen {
                family,
                span: span.clone(),
            });
        }

        // A group is defined once. That covers `up.begin … up.end … up.begin`,
        // and also bare statements followed by `up.begin`: those statements
        // already *are* the `up` content, so the block is redundant at best and
        // ambiguous at worst.
        if self.is_declared(family) || (family == GroupFamily::Up && !self.up.is_empty()) {
            return Err(DirectiveApplyError::GroupAlreadyDefined {
                family,
                span: span.clone(),
            });
        }

        if family == GroupFamily::Down {
            self.down_declared = true;
            self.down_begin_span.get_or_insert_with(|| span.clone());
        } else {
            self.up_declared = true;
        }

        self.open_blocks.push((family, span.clone()));
        Ok(())
    }

    /// Close a group. It must be the innermost one open.
    pub fn end_group(
        &mut self,
        family: GroupFamily,
        span: &Span,
    ) -> Result<(), DirectiveApplyError> {
        self.reject_orphan(span)?;
        self.note_first(span);

        let open = self.open_blocks.last().cloned();
        let Some((open_family, _)) = open else {
            return Err(DirectiveApplyError::CloseWithoutOpen {
                family,
                span: span.clone(),
            });
        };
        if open_family != family {
            return Err(DirectiveApplyError::MismatchedClose {
                expected: open_family,
                found: family,
                span: span.clone(),
            });
        }

        self.open_blocks.pop();
        Ok(())
    }

    /// Finish the file: close out the state and build the [`Migration`].
    ///
    /// `eof` is the position end-of-input reached, used when an empty `up` has
    /// no better location to point at.
    pub fn finish(self, eof: Span) -> Result<Migration, MigrationParseError> {
        // Reaching end of file with a block still open is a truncated file;
        // reaching it with *no* block ever opened is the ordinary case and must
        // not be treated the same way.
        if let Some((family, span)) = self.open_blocks.first() {
            return Err(MigrationParseError::Apply(
                DirectiveApplyError::UnclosedGroup {
                    family: *family,
                    span: span.clone(),
                },
            ));
        }
        if let Some(span) = self.pending_span.clone() {
            return Err(MigrationParseError::Apply(
                DirectiveApplyError::OrphanedStatementDirective { span },
            ));
        }

        if self.up.is_empty() {
            return Err(MigrationParseError::EmptyUp {
                span: self
                    .first_construct
                    .clone()
                    .unwrap_or_else(|| self.file_start.clone()),
            });
        }
        if self.down_declared && self.down.is_empty() {
            return Err(MigrationParseError::EmptyDown {
                span: self.down_begin_span.clone().unwrap_or_else(|| eof.clone()),
            });
        }

        Ok(Migration {
            up: StatementGroup::from_statements(self.up),
            down: self
                .down_declared
                .then(|| StatementGroup::from_statements(self.down)),
        })
    }

    /// A statement-level option may not survive a directive boundary — there is
    /// no statement left for it to apply to.
    fn reject_orphan(&self, boundary: &Span) -> Result<(), DirectiveApplyError> {
        // DECISION: the plan names block-`end` and end-of-file as the boundaries
        // that orphan an option. `begin` is checked too: an option queued just
        // before a group opens would otherwise be attached to the first
        // statement of a *different* group, which is not what was written.
        if self.pending_statement_options.is_empty() {
            return Ok(());
        }
        Err(DirectiveApplyError::OrphanedStatementDirective {
            span: self
                .pending_span
                .clone()
                .unwrap_or_else(|| boundary.clone()),
        })
    }

    fn is_declared(&self, family: GroupFamily) -> bool {
        match family {
            GroupFamily::Up => self.up_declared,
            GroupFamily::Down => self.down_declared,
        }
    }

    fn note_first(&mut self, span: &Span) {
        if self.first_construct.is_none() {
            self.first_construct = Some(span.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span_at(line: usize) -> Span {
        Span::new("m.sql", line, 1)
    }

    // Traceability to `db/src/migrations/PLAN.md`, "Verification →
    // Parser/context-level tests" and "Things to get right that are easy to get
    // wrong":
    //
    // | PLAN bullet | Test |
    // |---|---|
    // | `skipTx` attaches `OptSkipTx` to exactly the next statement | `a_pending_option_attaches_to_the_next_statement_only` |
    // | `skipTx` with no following statement before a block end or EOF is `OrphanedStatementDirective` | `an_option_followed_by_a_block_end_is_orphaned`, `an_option_at_end_of_file_is_orphaned` |
    // | `skipTx` twice before one statement — decided and tested | `the_same_option_queued_twice_appears_once` |
    // | `up.begin` without `up.end` before EOF is an error; likewise `down` | `up_begin_without_up_end_is_an_error`, `down_begin_without_down_end_is_an_error` |
    // | `up.end` with no preceding `up.begin` is an error | `ending_a_group_that_was_never_opened_is_an_error` |
    // | `up.begin` twice with no `up.end` is an error | `opening_a_group_twice_without_closing_it_is_an_error`, `a_group_cannot_be_declared_twice` |
    // | A file with no directives at all → everything in `up`, `down` is `None` | `no_blocks_at_all_is_not_an_error` (and `a_file_with_no_directives_puts_everything_in_up` in `tests/parse_migration_file.rs`) |
    // | A `down`-only file: what "up is required" means — decided and tested | `a_down_only_file_is_rejected_because_up_is_required` |
    // | Closing a family that was never opened / the wrong one | `ending_a_group_that_was_never_opened_is_an_error`, `closing_the_wrong_group_is_an_error` |
    // | EOF with **no** family open is not an error (implicit `up`) — not conflated with EOF with a family open | `no_blocks_at_all_is_not_an_error` vs `up_begin_without_up_end_is_an_error` |
    // | Pending options are cleared after every statement, so `skipTx` cannot leak | `pending_options_are_cleared_even_when_none_was_pending` |

    fn statement(sql: &str, line: usize) -> Statement {
        Statement::new(sql, span_at(line))
    }

    fn context() -> ParseContext {
        ParseContext::new(span_at(1))
    }

    /// `skipTx` immediately followed by a statement attaches `OptSkipTx` to
    /// exactly that statement.
    #[test]
    fn a_pending_option_attaches_to_the_next_statement_only() {
        let mut ctx = context();
        ctx.push_pending_option(StatementOption::OptSkipTx, &span_at(2));

        ctx.push_statement(statement("SELECT 1", 3));
        ctx.push_statement(statement("SELECT 2", 4));

        let migration = ctx.finish(span_at(5)).expect("one statement in up");
        assert!(migration.up.statements[0].has_option(StatementOption::OptSkipTx));
        assert!(
            migration.up.statements[1].options.is_empty(),
            "the option must not leak onto the next statement"
        );
    }

    // "easy to get wrong": pending options are cleared after every statement,
    // whether or not one was pending.
    #[test]
    fn pending_options_are_cleared_even_when_none_was_pending() {
        let mut ctx = context();
        ctx.push_pending_option(StatementOption::OptSkipTx, &span_at(1));
        ctx.push_statement(statement("SELECT 1", 2));
        assert!(ctx.pending_statement_options().is_empty());

        ctx.push_statement(statement("SELECT 2", 3));
        let migration = ctx.finish(span_at(4)).expect("parses");
        assert_eq!(migration.skip_tx_statements().len(), 1);
    }

    /// `skipTx` twice before one statement: the option is a set, so it appears
    /// once (see the DECISION on `push_pending_option`).
    #[test]
    fn the_same_option_queued_twice_appears_once() {
        let mut ctx = context();
        ctx.push_pending_option(StatementOption::OptSkipTx, &span_at(1));
        ctx.push_pending_option(StatementOption::OptSkipTx, &span_at(2));
        assert_eq!(ctx.pending_statement_options().len(), 1);

        ctx.push_statement(statement("SELECT 1", 3));
        let migration = ctx.finish(span_at(4)).expect("parses");
        assert_eq!(migration.up.statements[0].options.len(), 1);
    }

    #[test]
    fn an_option_followed_by_a_block_end_is_orphaned() {
        let mut ctx = context();
        ctx.begin_group(GroupFamily::Up, &span_at(1)).expect("open");
        ctx.push_pending_option(StatementOption::OptSkipTx, &span_at(2));
        let error = ctx
            .end_group(GroupFamily::Up, &span_at(3))
            .expect_err("no statement followed the directive");
        match error {
            DirectiveApplyError::OrphanedStatementDirective { span } => {
                assert_eq!(
                    span.line, 2,
                    "the orphan is the directive, not the boundary"
                );
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn an_option_at_end_of_file_is_orphaned() {
        let mut ctx = context();
        ctx.push_statement(statement("SELECT 1", 1));
        ctx.push_pending_option(StatementOption::OptSkipTx, &span_at(2));
        let error = ctx
            .finish(span_at(3))
            .expect_err("eof with a pending option");
        assert!(
            matches!(
                error,
                MigrationParseError::Apply(DirectiveApplyError::OrphanedStatementDirective { .. })
            ),
            "got {error:?}"
        );
    }

    #[test]
    fn an_option_before_a_block_begin_is_orphaned() {
        let mut ctx = context();
        ctx.push_statement(statement("SELECT 1", 1));
        ctx.push_pending_option(StatementOption::OptSkipTx, &span_at(2));
        let error = ctx
            .begin_group(GroupFamily::Down, &span_at(3))
            .expect_err("the option would cross into another group");
        assert!(
            matches!(
                error,
                DirectiveApplyError::OrphanedStatementDirective { .. }
            ),
            "got {error:?}"
        );
    }

    #[test]
    fn up_begin_without_up_end_is_an_error() {
        let mut ctx = context();
        ctx.begin_group(GroupFamily::Up, &span_at(1)).expect("open");
        ctx.push_statement(statement("SELECT 1", 2));
        let error = ctx.finish(span_at(3)).expect_err("unclosed up");
        match error {
            MigrationParseError::Apply(DirectiveApplyError::UnclosedGroup { family, span }) => {
                assert_eq!(family, GroupFamily::Up);
                assert_eq!(span.line, 1, "point at the begin, not the end of file");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn down_begin_without_down_end_is_an_error() {
        let mut ctx = context();
        ctx.push_statement(statement("SELECT 1", 1));
        ctx.begin_group(GroupFamily::Down, &span_at(2))
            .expect("open");
        ctx.push_statement(statement("DROP TABLE t", 3));
        let error = ctx.finish(span_at(4)).expect_err("unclosed down");
        assert!(
            matches!(
                error,
                MigrationParseError::Apply(DirectiveApplyError::UnclosedGroup {
                    family: GroupFamily::Down,
                    ..
                })
            ),
            "got {error:?}"
        );
    }

    #[test]
    fn ending_a_group_that_was_never_opened_is_an_error() {
        let mut ctx = context();
        let error = ctx
            .end_group(GroupFamily::Up, &span_at(5))
            .expect_err("nothing was open");
        match error {
            DirectiveApplyError::CloseWithoutOpen { family, span } => {
                assert_eq!(family, GroupFamily::Up);
                assert_eq!(span.line, 5);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn opening_a_group_twice_without_closing_it_is_an_error() {
        let mut ctx = context();
        ctx.begin_group(GroupFamily::Up, &span_at(1)).expect("open");
        let error = ctx
            .begin_group(GroupFamily::Up, &span_at(2))
            .expect_err("up is already open");
        assert!(
            matches!(
                error,
                DirectiveApplyError::DuplicateGroupOpen {
                    family: GroupFamily::Up,
                    ..
                }
            ),
            "got {error:?}"
        );
    }

    #[test]
    fn closing_the_wrong_group_is_an_error() {
        let mut ctx = context();
        ctx.begin_group(GroupFamily::Up, &span_at(1)).expect("open");
        let error = ctx
            .end_group(GroupFamily::Down, &span_at(2))
            .expect_err("up is what is open");
        match error {
            DirectiveApplyError::MismatchedClose {
                expected, found, ..
            } => {
                assert_eq!(expected, GroupFamily::Up);
                assert_eq!(found, GroupFamily::Down);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_group_cannot_be_declared_twice() {
        let mut ctx = context();
        ctx.begin_group(GroupFamily::Up, &span_at(1)).expect("open");
        ctx.push_statement(statement("SELECT 1", 2));
        ctx.end_group(GroupFamily::Up, &span_at(3)).expect("close");
        let error = ctx
            .begin_group(GroupFamily::Up, &span_at(4))
            .expect_err("up is already defined");
        assert!(
            matches!(
                error,
                DirectiveApplyError::GroupAlreadyDefined {
                    family: GroupFamily::Up,
                    ..
                }
            ),
            "got {error:?}"
        );
    }

    #[test]
    fn bare_statements_then_up_begin_conflicts() {
        // The bare statements already *are* the up content; a later `up.begin`
        // cannot also define it.
        let mut ctx = context();
        ctx.push_statement(statement("SELECT 1", 1));
        let error = ctx
            .begin_group(GroupFamily::Up, &span_at(2))
            .expect_err("up content already exists");
        assert!(
            matches!(error, DirectiveApplyError::GroupAlreadyDefined { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn bare_statements_before_a_down_block_belong_to_up() {
        let mut ctx = context();
        ctx.push_statement(statement("CREATE TABLE t (id INT)", 1));
        ctx.begin_group(GroupFamily::Down, &span_at(2))
            .expect("open");
        ctx.push_statement(statement("DROP TABLE t", 3));
        ctx.end_group(GroupFamily::Down, &span_at(4))
            .expect("close");

        let migration = ctx.finish(span_at(5)).expect("well formed");
        assert_eq!(migration.up.statements.len(), 1);
        assert_eq!(migration.up.statements[0].sql, "CREATE TABLE t (id INT)");
        assert_eq!(migration.down.expect("declared").statements.len(), 1);
    }

    #[test]
    fn a_down_block_nested_in_up_keeps_both_statements() {
        let mut ctx = context();
        ctx.begin_group(GroupFamily::Up, &span_at(1)).expect("open");
        ctx.push_statement(statement("SELECT 1", 2));
        ctx.begin_group(GroupFamily::Down, &span_at(3))
            .expect("nested open");
        ctx.push_statement(statement("SELECT 2", 4));
        ctx.end_group(GroupFamily::Down, &span_at(5))
            .expect("nested close");
        ctx.push_statement(statement("SELECT 3", 6));
        ctx.end_group(GroupFamily::Up, &span_at(7)).expect("close");

        let migration = ctx.finish(span_at(8)).expect("well formed");
        assert_eq!(
            migration
                .up
                .statements
                .iter()
                .map(|s| s.sql.as_str())
                .collect::<Vec<_>>(),
            vec!["SELECT 1", "SELECT 3"],
            "statements either side of a nested down block stay in up"
        );
        assert_eq!(migration.down.expect("declared").statements.len(), 1);
    }

    /// The plan asks what "up is required" means for a `down`-only file: it is an
    /// error.
    // DECISION: an `up` with no statements would still be recorded as applied by
    // the migrator while changing nothing, leaving the database claiming a schema
    // it does not have — the same failure the crate already refuses for an empty
    // `down` block.
    #[test]
    fn a_down_only_file_is_rejected_because_up_is_required() {
        let mut ctx = context();
        ctx.begin_group(GroupFamily::Down, &span_at(1))
            .expect("open");
        ctx.push_statement(statement("DROP TABLE t", 2));
        ctx.end_group(GroupFamily::Down, &span_at(3))
            .expect("close");

        let error = ctx.finish(span_at(4)).expect_err("up has no statements");
        match error {
            MigrationParseError::EmptyUp { span } => {
                assert_eq!(span.line, 1, "points at the first construct in the file");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_declared_but_empty_down_is_rejected() {
        let mut ctx = context();
        ctx.push_statement(statement("CREATE TABLE t (id INT)", 1));
        ctx.begin_group(GroupFamily::Down, &span_at(2))
            .expect("open");
        ctx.end_group(GroupFamily::Down, &span_at(3))
            .expect("close");

        let error = ctx.finish(span_at(4)).expect_err("down has no statements");
        match error {
            MigrationParseError::EmptyDown { span } => {
                assert_eq!(span.line, 2, "points at down.begin");
            }
            other => panic!("got {other:?}"),
        }
    }

    /// No directives at all is the ordinary case, not an error: this is the
    /// fallback every existing migration file depends on.
    #[test]
    fn no_blocks_at_all_is_not_an_error() {
        let mut ctx = context();
        ctx.push_statement(statement("SELECT 1", 1));
        ctx.push_statement(statement("SELECT 2", 2));

        let migration = ctx.finish(span_at(3)).expect("implicit up");
        assert_eq!(migration.up.statements.len(), 2);
        assert!(migration.down.is_none(), "no down was declared");
    }

    #[test]
    fn a_file_with_no_statements_at_all_is_rejected() {
        let error = context().finish(span_at(1)).expect_err("nothing in up");
        assert!(
            matches!(error, MigrationParseError::EmptyUp { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn active_tracks_the_innermost_open_block() {
        let mut ctx = context();
        assert_eq!(ctx.active(), None);
        ctx.begin_group(GroupFamily::Up, &span_at(1)).expect("open");
        assert_eq!(ctx.active(), Some(GroupFamily::Up));
        ctx.begin_group(GroupFamily::Down, &span_at(2))
            .expect("nested");
        assert_eq!(ctx.active(), Some(GroupFamily::Down));
        ctx.end_group(GroupFamily::Down, &span_at(3))
            .expect("close");
        assert_eq!(ctx.active(), Some(GroupFamily::Up));
    }
}
