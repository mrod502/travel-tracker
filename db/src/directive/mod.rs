//! What a directive means, as opposed to how the lexer spelled it.
//!
//! A directive is split into two concerns:
//!
//! * **scope** — how it attaches structurally. [`DirectiveScope::Begin`] and
//!   [`DirectiveScope::End`] open and close a group and are handled entirely by
//!   the parser core, which is why `up.begin` and friends carry no logic of
//!   their own. [`DirectiveScope::Standalone`] fires once and mutates parser
//!   state through [`Directive::apply`].
//! * **effect** — what it actually does, which for a standalone directive is its
//!   `apply`.
//!
//! Keys are a [`DirectiveKey`] newtype over `&'static str` rather than a closed
//! enum, so a new directive registers itself without editing a central `match`.

pub mod args;
pub mod builtin;
pub mod registry;

use std::fmt;

use crate::error::{DirectiveApplyError, DirectiveParseError};
use crate::lex::Span;
use crate::parse::ParseContext;

use args::{Args, parse_args};

/// Uniquely identifies a directive.
///
/// A newtype over `&'static str` — not a closed enum — so directives can be
/// registered without editing a central match. The key is the dotted path as
/// written: `up.begin`, `skipTx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DirectiveKey(pub &'static str);

/// Which group of a migration a [`DirectiveScope::Begin`]/[`DirectiveScope::End`]
/// opens or closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupFamily {
    /// The forward direction, [`Migration::up`](crate::model::Migration::up).
    Up,
    /// The revert, [`Migration::down`](crate::model::Migration::down).
    Down,
}

impl GroupFamily {
    /// The name as written in a directive: `up` or `down`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

impl fmt::Display for GroupFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a standalone directive applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandaloneTarget {
    /// Applies only to the next statement parsed.
    ///
    /// If a block directive or end of file is reached with no statement in
    /// between, that is
    /// [`DirectiveApplyError::OrphanedStatementDirective`] — an option with
    /// nothing to apply to is a mistake, not a no-op.
    NextStatement,
    /// Applies to every statement in the enclosing group.
    ///
    /// Reserved: no directive in this version uses it. A directive that did
    /// would have to ensure no statement had been accumulated in the current
    /// group yet, since an option cannot retroactively cover one.
    EnclosingGroup,
}

/// How a directive attaches to the surrounding SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectiveScope {
    /// Opens a region: statements belong to `family` until the matching
    /// [`DirectiveScope::End`]. Handled by the parser core.
    Begin {
        /// The group opened.
        family: GroupFamily,
    },
    /// Closes the region opened by the matching
    /// [`DirectiveScope::Begin`]. Handled by the parser core.
    End {
        /// The group closed.
        family: GroupFamily,
    },
    /// Fires once, immediately, through [`Directive::apply`].
    Standalone(StandaloneTarget),
}

/// A migration-file directive.
///
/// Implementations are cheap values: the registry parses one per occurrence and
/// hands it to the parser core, which asks for its [`Directive::scope`] and, for
/// a standalone directive, calls [`Directive::apply`].
///
/// `parse` returns `Self`, so this trait is not object-safe on its own;
/// [`DirectiveObj`] is the dynamic-dispatch view the registry stores.
pub trait Directive: fmt::Debug {
    /// The key this directive registers under.
    fn key() -> DirectiveKey
    where
        Self: Sized;

    /// Build the directive from its parsed arguments.
    ///
    /// `span` is the `--` that opens the directive, for the error a rejection
    /// carries.
    fn parse(args: &Args, span: &Span) -> Result<Self, DirectiveParseError>
    where
        Self: Sized;

    /// A method rather than an associated function so an argument-driven variant
    /// (say `skipTx(scope="group")`) could pick its scope dynamically.
    fn scope(&self) -> DirectiveScope;

    /// Mutate parser state.
    ///
    /// Defaults to doing nothing: `Begin`/`End` directives never need it, since
    /// the parser core pushes and pops groups off [`Directive::scope`] alone.
    /// Standalone directives override it.
    ///
    /// `span` is where the directive was written, so a rejection can name a
    /// location.
    fn apply(&self, _ctx: &mut ParseContext, _span: &Span) -> Result<(), DirectiveApplyError> {
        Ok(())
    }
}

/// The dynamic-dispatch view of a parsed directive.
///
/// Blanket-implemented for every [`Directive`], so implementors never touch it;
/// it exists because `Directive::parse` returns `Self` and is therefore not
/// object-safe. Implementing `DirectiveObj` directly on a type that is not a
/// `Directive` is not supported — the blanket impl claims it.
pub trait DirectiveObj: fmt::Debug {
    /// See [`Directive::scope`].
    fn scope(&self) -> DirectiveScope;

    /// See [`Directive::apply`].
    fn apply(&self, ctx: &mut ParseContext, span: &Span) -> Result<(), DirectiveApplyError>;
}

impl<T: Directive> DirectiveObj for T {
    fn scope(&self) -> DirectiveScope {
        Directive::scope(self)
    }

    fn apply(&self, ctx: &mut ParseContext, span: &Span) -> Result<(), DirectiveApplyError> {
        Directive::apply(self, ctx, span)
    }
}

/// Reject an argument list a directive does not accept.
///
/// Built-in directives use this; so should any directive added downstream that
/// takes no arguments — otherwise `--migrate:skipTx(scope="group")` would parse
/// as though the argument meant something today.
pub fn require_no_args(args: &Args, key: &str, span: &Span) -> Result<(), DirectiveParseError> {
    if args.is_empty() {
        return Ok(());
    }
    Err(DirectiveParseError::UnexpectedArguments {
        key: key.to_string(),
        span: span.clone(),
    })
}

/// Parse the arguments of a directive occurrence, as the registry does before
/// calling [`Directive::parse`].
pub(crate) fn arguments_of(
    raw: &crate::lex::RawDirective<'_>,
) -> Result<Args, DirectiveParseError> {
    let text = raw.args.unwrap_or("");
    Ok(parse_args(text, &raw.span)?)
}

pub use builtin::skip_tx::SkipTx;
pub use builtin::up_down::{DownBegin, DownEnd, UpBegin, UpEnd};
pub use registry::DirectiveRegistry;

#[cfg(test)]
mod tests {
    use super::args::ArgValue;
    use super::*;
    use crate::model::StatementOption;

    #[test]
    fn group_family_names_match_the_directives() {
        assert_eq!(GroupFamily::Up.as_str(), "up");
        assert_eq!(GroupFamily::Down.to_string(), "down");
    }

    /// A directive that records that `apply` ran, to check the blanket impl and
    /// the default no-op.
    #[derive(Debug)]
    struct Recorder;

    impl Directive for Recorder {
        fn key() -> DirectiveKey {
            DirectiveKey("recorder")
        }

        fn parse(args: &Args, span: &Span) -> Result<Self, DirectiveParseError> {
            require_no_args(args, "recorder", span)?;
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

    #[test]
    fn blanket_impl_exposes_scope_and_apply() {
        let span = Span::in_memory(1, 1);
        let parsed = Recorder::parse(&Args::empty(), &span).expect("no args expected");
        let object: Box<dyn DirectiveObj> = Box::new(parsed);
        assert_eq!(
            object.scope(),
            DirectiveScope::Standalone(StandaloneTarget::NextStatement)
        );

        let mut ctx = ParseContext::new(span.clone());
        object.apply(&mut ctx, &span).expect("apply ok");
        assert_eq!(
            ctx.pending_statement_options(),
            &[StatementOption::OptSkipTx]
        );
    }

    #[test]
    fn begin_directives_use_the_default_no_op_apply() {
        let span = Span::in_memory(1, 1);
        let begin = UpBegin::parse(&Args::empty(), &span).expect("no args expected");
        let mut ctx = ParseContext::new(span.clone());
        // Fully qualified: `Directive` and `DirectiveObj` both expose `apply`,
        // so a bare method call on a concrete type would be ambiguous.
        DirectiveObj::apply(&begin, &mut ctx, &span).expect("no-op apply succeeds");
        assert!(ctx.pending_statement_options().is_empty());
    }

    #[test]
    fn arguments_are_rejected_where_a_directive_takes_none() {
        let span = Span::in_memory(2, 5);
        let args = parse_args("scope=\"group\"", &span).expect("args parse");
        let error =
            require_no_args(&args, "skipTx", &span).expect_err("arguments should be rejected");
        match error {
            DirectiveParseError::UnexpectedArguments { key, span: at } => {
                assert_eq!(key, "skipTx");
                assert_eq!(at, span);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn arg_value_text_is_available_whatever_the_spelling() {
        let args = parse_args("bare, \"quoted\"", &Span::in_memory(1, 1)).expect("parses");
        assert_eq!(args.get(0).map(ArgValue::as_str), Some("bare"));
        assert_eq!(args.get(1).map(ArgValue::as_str), Some("quoted"));
    }
}
