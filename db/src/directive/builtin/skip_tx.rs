//! `--migrate:skipTx` — run the next statement outside the transaction.
//!
//! PostgreSQL refuses `CREATE INDEX CONCURRENTLY` inside a transaction block, so
//! a migration that needs one has to say which statement leaves the transaction.
//! That statement cannot be wrapped in the same implicit "begin … commit" as its
//! neighbours, and the statement after it must not inherit the exemption:
//! getting that wrong either fails the migration or quietly runs a statement
//! that was meant to be atomic.

use crate::directive::args::Args;
use crate::directive::{
    Directive, DirectiveKey, DirectiveScope, StandaloneTarget, require_no_args,
};
use crate::error::{DirectiveApplyError, DirectiveParseError};
use crate::lex::Span;
use crate::model::StatementOption;
use crate::parse::ParseContext;

/// `--migrate:skipTx`: attach [`StatementOption::OptSkipTx`] to the statement
/// that follows.
///
/// Scope is [`StandaloneTarget::NextStatement`], so the option is queued on the
/// [`ParseContext`] and drained by exactly one
/// [`push_statement`](ParseContext::push_statement). A block directive or end of
/// file reached before any statement is an
/// [`OrphanedStatementDirective`](DirectiveApplyError::OrphanedStatementDirective),
/// never a silently dropped option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkipTx;

impl Directive for SkipTx {
    fn key() -> DirectiveKey {
        DirectiveKey("skipTx")
    }

    fn parse(args: &Args, span: &Span) -> Result<Self, DirectiveParseError> {
        // DECISION: `skipTx` takes no arguments. `skipTx(scope="group")` is the
        // future group-wide form named in the plan; until it exists it is an
        // error here rather than an accepted spelling of "not implemented".
        require_no_args(args, "skipTx", span)?;
        Ok(Self)
    }

    fn scope(&self) -> DirectiveScope {
        DirectiveScope::Standalone(StandaloneTarget::NextStatement)
    }

    fn apply(&self, ctx: &mut ParseContext, span: &Span) -> Result<(), DirectiveApplyError> {
        ctx.push_pending_option(StatementOption::OptSkipTx, span);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_and_scope() {
        assert_eq!(SkipTx::key().0, "skipTx");
        assert_eq!(
            SkipTx.scope(),
            DirectiveScope::Standalone(StandaloneTarget::NextStatement)
        );
    }

    #[test]
    fn apply_queues_the_option() {
        let span = Span::in_memory(3, 1);
        let mut ctx = ParseContext::new(span.clone());
        SkipTx.apply(&mut ctx, &span).expect("queuing cannot fail");
        assert_eq!(
            ctx.pending_statement_options(),
            &[StatementOption::OptSkipTx]
        );
    }

    #[test]
    fn arguments_are_rejected() {
        let span = Span::in_memory(1, 1);
        let args = crate::directive::args::parse_args("group", &span).expect("arg list parses");
        let error = SkipTx::parse(&args, &span).expect_err("skipTx takes no arguments");
        assert!(
            matches!(&error, DirectiveParseError::UnexpectedArguments { key, .. } if key == "skipTx"),
            "got {error:?}"
        );
    }
}
