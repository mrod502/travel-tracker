//! `up.begin` / `up.end` / `down.begin` / `down.end` — the block directives.
//!
//! All four are [`DirectiveScope::Begin`] or [`DirectiveScope::End`], which the
//! parser core pushes and pops generically; none of them implements
//! [`Directive::apply`], which is the point of splitting scope from effect.

use crate::directive::args::Args;
use crate::directive::{Directive, DirectiveKey, DirectiveScope, GroupFamily, require_no_args};
use crate::error::DirectiveParseError;
use crate::lex::Span;

/// `--migrate:up.begin` — statements after this belong to
/// [`Migration::up`](crate::model::Migration::up) until [`UpEnd`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpBegin;

/// `--migrate:up.end` — closes the `up` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpEnd;

/// `--migrate:down.begin` — statements after this belong to
/// [`Migration::down`](crate::model::Migration::down) until [`DownEnd`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownBegin;

/// `--migrate:down.end` — closes the `down` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownEnd;

impl Directive for UpBegin {
    fn key() -> DirectiveKey {
        DirectiveKey("up.begin")
    }

    fn parse(args: &Args, span: &Span) -> Result<Self, DirectiveParseError> {
        // DECISION: `up.begin(skipTx)` is rejected for now. The plan reserves
        // group-wide options for a later version; accepting the syntax here
        // without implementing it would let a file ask for something that is
        // silently ignored.
        require_no_args(args, "up.begin", span)?;
        Ok(Self)
    }

    fn scope(&self) -> DirectiveScope {
        DirectiveScope::Begin {
            family: GroupFamily::Up,
        }
    }
}

impl Directive for UpEnd {
    fn key() -> DirectiveKey {
        DirectiveKey("up.end")
    }

    fn parse(args: &Args, span: &Span) -> Result<Self, DirectiveParseError> {
        require_no_args(args, "up.end", span)?;
        Ok(Self)
    }

    fn scope(&self) -> DirectiveScope {
        DirectiveScope::End {
            family: GroupFamily::Up,
        }
    }
}

impl Directive for DownBegin {
    fn key() -> DirectiveKey {
        DirectiveKey("down.begin")
    }

    fn parse(args: &Args, span: &Span) -> Result<Self, DirectiveParseError> {
        require_no_args(args, "down.begin", span)?;
        Ok(Self)
    }

    fn scope(&self) -> DirectiveScope {
        DirectiveScope::Begin {
            family: GroupFamily::Down,
        }
    }
}

impl Directive for DownEnd {
    fn key() -> DirectiveKey {
        DirectiveKey("down.end")
    }

    fn parse(args: &Args, span: &Span) -> Result<Self, DirectiveParseError> {
        require_no_args(args, "down.end", span)?;
        Ok(Self)
    }

    fn scope(&self) -> DirectiveScope {
        DirectiveScope::End {
            family: GroupFamily::Down,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span() -> Span {
        Span::in_memory(1, 1)
    }

    #[test]
    fn keys_are_the_dotted_paths() {
        assert_eq!(UpBegin::key().0, "up.begin");
        assert_eq!(UpEnd::key().0, "up.end");
        assert_eq!(DownBegin::key().0, "down.begin");
        assert_eq!(DownEnd::key().0, "down.end");
    }

    #[test]
    fn scopes_open_and_close_the_right_family() {
        assert_eq!(
            UpBegin.scope(),
            DirectiveScope::Begin {
                family: GroupFamily::Up
            }
        );
        assert_eq!(
            UpEnd.scope(),
            DirectiveScope::End {
                family: GroupFamily::Up
            }
        );
        assert_eq!(
            DownBegin.scope(),
            DirectiveScope::Begin {
                family: GroupFamily::Down
            }
        );
        assert_eq!(
            DownEnd.scope(),
            DirectiveScope::End {
                family: GroupFamily::Down
            }
        );
    }

    #[test]
    fn empty_or_absent_arguments_are_accepted() {
        for args in [Args::empty(), parse_args_text("").expect("empty")] {
            assert!(UpBegin::parse(&args, &span()).is_ok());
            assert!(DownEnd::parse(&args, &span()).is_ok());
        }
    }

    #[test]
    fn arguments_are_rejected() {
        let args = parse_args_text("skipTx").expect("parses as an argument list");
        let error = UpBegin::parse(&args, &span()).expect_err("up.begin takes no arguments");
        assert!(
            matches!(error, DirectiveParseError::UnexpectedArguments { .. }),
            "got {error:?}"
        );
    }

    fn parse_args_text(text: &str) -> Result<Args, DirectiveParseError> {
        crate::directive::args::parse_args(text, &span()).map_err(DirectiveParseError::from)
    }
}
