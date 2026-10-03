//! The driving loop: lexer → registry → [`ParseContext`] → [`Migration`].

mod context;

pub use context::ParseContext;

use std::path::{Path, PathBuf};

use crate::directive::DirectiveRegistry;
use crate::directive::DirectiveScope;
use crate::error::MigrationParseError;
use crate::lex::{LexToken, Lexer, Span, end_span};
use crate::model::Migration;

/// Turns the text of a migration file into a [`Migration`].
///
/// A trait so a caller can supply its own parser (a file-format variant, a
/// recorder for a dry run) while holding the same output type.
pub trait MigrationParser {
    /// Parse `source` using the directives `registry` knows.
    ///
    /// Every failure is a [`MigrationParseError`] carrying the position it came
    /// from; none of the input-driven paths panic.
    ///
    /// ```
    /// use db::{DirectiveRegistry, MigrationParser, StandardMigrationParser};
    ///
    /// let source = "\
    /// --migrate:up.begin
    /// CREATE TABLE users (id INT);
    /// --migrate:up.end
    /// --migrate:down.begin
    /// DROP TABLE users;
    /// --migrate:down.end
    /// ";
    /// let registry = DirectiveRegistry::with_builtins()?;
    /// let migration = StandardMigrationParser::in_memory().parse(source, &registry)?;
    ///
    /// assert_eq!(migration.up.statements.len(), 1);
    /// assert_eq!(migration.down.as_ref().map(|g| g.len()), Some(1));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    fn parse(
        &self,
        source: &str,
        registry: &DirectiveRegistry,
    ) -> Result<Migration, MigrationParseError>;
}

/// The parser for the format described in `db/src/migrations/PLAN.md`.
///
/// One instance per file: the path is carried so every span and error can name
/// the file it came from. [`StandardMigrationParser::in_memory`] is the same
/// parser for a source that is not on disk — spans then print `<input>` — which
/// is what tests and string-based callers want.
#[derive(Debug, Clone)]
pub struct StandardMigrationParser {
    file: PathBuf,
}

impl StandardMigrationParser {
    /// A parser whose errors report positions in `file`.
    pub fn new(file: impl Into<PathBuf>) -> Self {
        Self { file: file.into() }
    }

    /// A parser for a source with no file behind it.
    pub fn in_memory() -> Self {
        Self {
            file: PathBuf::new(),
        }
    }

    /// The file this parser attributes positions to.
    pub fn file(&self) -> &Path {
        &self.file
    }
}

impl MigrationParser for StandardMigrationParser {
    /// Runs the lexer, resolves each directive against `registry`, and lets the
    /// directive's [`DirectiveScope`] decide who handles it: the parser core
    /// pushes and pops groups, a standalone directive mutates the context.
    fn parse(
        &self,
        source: &str,
        registry: &DirectiveRegistry,
    ) -> Result<Migration, MigrationParseError> {
        let mut ctx = ParseContext::new(Span::new(self.file.clone(), 1, 1));

        for token in Lexer::new(source, self.file.clone()) {
            match token? {
                LexToken::Statement(statement) => ctx.push_statement(statement),
                LexToken::Directive(raw) => {
                    let directive = registry.resolve(&raw)?;
                    match directive.scope() {
                        DirectiveScope::Begin { family } => ctx.begin_group(family, &raw.span)?,
                        DirectiveScope::End { family } => ctx.end_group(family, &raw.span)?,
                        DirectiveScope::Standalone(_) => directive.apply(&mut ctx, &raw.span)?,
                    }
                }
            }
        }

        // `finish` is what distinguishes the two end-of-file cases: a block left
        // open is an error, no block ever opened is the implicit-`up` fallback.
        ctx.finish(end_span(source, &self.file))
    }
}
