//! Library surface of the `db` crate: the migration-file parser.
//!
//! A migration file is plain SQL plus a small set of directives written in
//! comments of the form `--migrate:<path>[(<args>)]`. The parser turns such a
//! file into a [`Migration`]: an `up` [`StatementGroup`] plus an optional
//! `down` group, with per-statement options such as
//! [`StatementOption::OptSkipTx`] attached where the file asked for them.
//!
//! ```
//! use db::{DirectiveRegistry, MigrationParser, StandardMigrationParser};
//!
//! let source = "--migrate:up.begin\nCREATE TABLE users (id INT);\n--migrate:up.end\n";
//! let registry = DirectiveRegistry::with_builtins().expect("built-in keys are distinct");
//! let migration = StandardMigrationParser::in_memory()
//!     .parse(source, &registry)
//!     .expect("valid migration");
//!
//! assert_eq!(migration.up.statements.len(), 1);
//! assert!(migration.down.is_none());
//! ```
//!
//! # Layers
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`lex`] | One character-level pass that splits SQL into [`Statement`]s on top-level `;` and lifts `--migrate:` lines out as [`RawDirective`]s. Knows nothing about what a directive means. |
//! | [`directive`] | What a directive key means: [`DirectiveScope`], argument parsing, the [`DirectiveRegistry`], and the built-ins. |
//! | [`parse`] | [`ParseContext`] (group nesting, pending statement options) and the [`MigrationParser`] driving loop that joins the two. |
//! | [`model`] | The output: [`Statement`], [`StatementGroup`], [`Migration`]. |
//!
//! [`error`] carries every failure mode as a typed variant. Errors raised while
//! lexing or applying a directive carry the [`Span`] they came from so a CLI can
//! print `file:line:col: message`; none of these paths panic on bad input.
//!
//! # Consuming a parse
//!
//! A caller that applies a [`Migration`] to a database does not need to know
//! which directive produced an option — only that some statements must not run
//! inside the surrounding transaction:
//!
//! ```
//! use db::{DirectiveRegistry, MigrationParser, StandardMigrationParser, StatementOption};
//!
//! # fn in_transaction(sql: &str) -> Result<(), Box<dyn std::error::Error>> {
//! #     let _ = sql; Ok(())
//! # }
//! # fn outside_transaction(sql: &str) -> Result<(), Box<dyn std::error::Error>> {
//! #     let _ = sql; Ok(())
//! # }
//! # fn example(source: &str) -> Result<(), Box<dyn std::error::Error>> {
//! let registry = DirectiveRegistry::with_builtins()?;
//! let migration = StandardMigrationParser::new("202601010000_add_users.sql")
//!     .parse(source, &registry)?;
//!
//! for statement in &migration.up.statements {
//!     if statement.has_option(StatementOption::OptSkipTx) {
//!         outside_transaction(&statement.sql)?;
//!     } else {
//!         in_transaction(&statement.sql)?;
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! A statement that must leave the transaction is also available as a set via
//! [`Migration::skip_tx_statements`], which is what a dry-run report wants.
//!
//! [`Migration::down`] is `None` unless the file declared a `down` block, which
//! is how a caller tells "no revert exists" from "an empty revert" — the latter
//! is rejected at parse time. A failing [`MigrationParseError`] formats as
//! `file:line:col: message` through its [`std::fmt::Display`], and
//! [`MigrationParseError::span`] exposes the location structurally.
//!
//! The `db` binary is the reference consumer: `db up` applies [`Migration::up`],
//! `db down` applies [`Migration::down`], and both honour
//! [`StatementOption::OptSkipTx`] (see `db/src/exec.rs`).
//!
//! [`forward`] is the other kind of consumer: a projection of the corpus onto
//! sqlx's own [`Migrator`](sqlx::migrate::Migrator), `up` statements only, for
//! code that runs migrations through sqlx — `#[sqlx::test]` in sibling crates —
//! instead of through this binary.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod error;
pub mod lex;
pub mod parse;

pub mod directive;
pub mod forward;
pub mod model;

pub use directive::{
    Directive, DirectiveKey, DirectiveObj, DirectiveScope, GroupFamily, StandaloneTarget,
    registry::DirectiveRegistry,
};
pub use error::{
    ArgError, DirectiveApplyError, DirectiveParseError, LexError, MigrationParseError,
    RegistrationError,
};
pub use forward::{ForwardMigratorError, MIGRATIONS_DIR, SQLX_FORWARD_MIGRATOR, forward_migrator};
pub use lex::{LexState, LexToken, Lexer, RawDirective, Span};
pub use model::{Migration, Statement, StatementGroup, StatementOption};
pub use parse::{MigrationParser, ParseContext, StandardMigrationParser};
