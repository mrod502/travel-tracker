//! Resolving a directive key to the code that implements it.

use std::collections::HashMap;

use crate::error::{DirectiveParseError, RegistrationError};
use crate::lex::RawDirective;

use super::Directive;
use super::DirectiveObj;
use super::arguments_of;
use super::builtin;

/// The type-erased form of a registered directive: given the raw directive the
/// lexer produced, parse its arguments and hand back the implementation as a
/// [`DirectiveObj`], so the parse loop never needs the concrete type.
type Constructor =
    Box<dyn Fn(&RawDirective<'_>) -> Result<Box<dyn DirectiveObj>, DirectiveParseError>>;

/// The set of directives a parse understands.
///
/// A file that uses a key the registry does not have fails the parse with
/// [`DirectiveParseError::UnknownDirective`] rather than ignoring the line: a
/// misspelled `up.bgin` silently treated as a comment would produce a migration
/// whose shape is not what the author wrote.
///
/// ```
/// use db::StandaloneTarget;
/// use db::directive::args::Args;
/// use db::directive::{Directive, DirectiveKey, DirectiveRegistry, DirectiveScope};
/// use db::{DirectiveApplyError, DirectiveParseError, ParseContext, RawDirective, Span};
///
/// #[derive(Debug)]
/// struct Freeze;
///
/// impl Directive for Freeze {
///     fn key() -> DirectiveKey {
///         DirectiveKey("freeze")
///     }
///     fn parse(args: &Args, span: &Span) -> Result<Self, DirectiveParseError> {
///         db::directive::require_no_args(args, "freeze", span)?;
///         Ok(Freeze)
///     }
///     fn scope(&self) -> DirectiveScope {
///         DirectiveScope::Standalone(StandaloneTarget::NextStatement)
///     }
///     fn apply(&self, ctx: &mut ParseContext, span: &Span) -> Result<(), DirectiveApplyError> {
///         ctx.push_pending_option(db::StatementOption::OptSkipTx, span);
///         Ok(())
///     }
/// }
///
/// let mut registry = DirectiveRegistry::new();
/// registry.register::<Freeze>()?;
/// assert!(registry.contains("freeze"));
///
/// let raw = RawDirective {
///     path: vec!["thaw"],
///     args: None,
///     span: Span::in_memory(1, 1),
/// };
/// let error = registry.resolve(&raw).err();
/// assert!(matches!(
///     error,
///     Some(DirectiveParseError::UnknownDirective { ref key, .. }) if key == "thaw"
/// ));
/// # Ok::<(), db::RegistrationError>(())
/// ```
// No `Clone`: the entries hold boxed closures, which are not cloneable, and a
// registry is built once at start-up and shared by reference.
#[derive(Default)]
pub struct DirectiveRegistry {
    entries: HashMap<&'static str, Constructor>,
}

impl std::fmt::Debug for DirectiveRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectiveRegistry")
            .field("keys", &self.keys())
            .finish()
    }
}

impl DirectiveRegistry {
    /// An empty registry: a parse against it accepts no directives, so a file
    /// with any `--migrate:` line in it is an unknown-directive error.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// A registry holding every directive the library ships with — `up.begin`,
    /// `up.end`, `down.begin`, `down.end`, `skipTx`. This is what a CLI should
    /// parse with.
    ///
    /// Only fails if two built-ins were given the same key, which is checked
    /// here rather than with an `expect`.
    pub fn with_builtins() -> Result<Self, RegistrationError> {
        let mut registry = Self::new();
        builtin::register_all(&mut registry)?;
        Ok(registry)
    }

    /// Register `D` under its own key.
    ///
    /// The type parameter is what binds a key to its implementation; the stored
    /// closure parses the arguments and boxes the result as a
    /// [`DirectiveObj`] so the parser core never needs the concrete type.
    pub fn register<D>(&mut self) -> Result<(), RegistrationError>
    where
        D: Directive + 'static,
    {
        // DECISION: registering the same key twice is rejected, not
        // last-write-wins. Two directives claiming `skipTx` is a wiring mistake in
        // the host application, and one that silently changes which one runs is
        // worse than a start-up failure.
        let key = D::key();
        if self.entries.contains_key(key.0) {
            return Err(RegistrationError {
                key: key.0.to_string(),
            });
        }
        self.entries.insert(
            key.0,
            Box::new(|raw: &RawDirective<'_>| {
                let args = arguments_of(raw)?;
                D::parse(&args, &raw.span)
                    .map(|directive| Box::new(directive) as Box<dyn DirectiveObj>)
            }),
        );
        Ok(())
    }

    /// Turn a lexed directive into its implementation.
    ///
    /// The dotted path is the lookup key (`["up", "begin"]` → `up.begin`). An
    /// unknown key is reported with both the key and the span, so the message a
    /// CLI prints names what was wrong and where.
    pub fn resolve(
        &self,
        raw: &RawDirective<'_>,
    ) -> Result<Box<dyn DirectiveObj>, DirectiveParseError> {
        let key = raw.key();
        let entry = self.entries.get(key.as_str()).ok_or_else(|| {
            DirectiveParseError::UnknownDirective {
                key: key.clone(),
                span: raw.span.clone(),
            }
        })?;
        entry(raw)
    }

    /// Is `key` registered?
    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// The registered keys, sorted, for diagnostics ("did you mean …?").
    pub fn keys(&self) -> Vec<&'static str> {
        let mut keys: Vec<&'static str> = self.entries.keys().copied().collect();
        keys.sort_unstable();
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directive::DirectiveKey;
    use crate::directive::args::Args;
    use crate::directive::{DirectiveScope, StandaloneTarget};
    use crate::lex::Span;

    #[derive(Debug)]
    struct Alpha;

    impl Directive for Alpha {
        fn key() -> DirectiveKey {
            DirectiveKey("alpha")
        }
        fn parse(_args: &Args, _span: &Span) -> Result<Self, DirectiveParseError> {
            Ok(Self)
        }
        fn scope(&self) -> DirectiveScope {
            DirectiveScope::Standalone(StandaloneTarget::NextStatement)
        }
    }

    #[derive(Debug)]
    struct Beta;

    impl Directive for Beta {
        fn key() -> DirectiveKey {
            DirectiveKey("beta")
        }
        fn parse(_args: &Args, _span: &Span) -> Result<Self, DirectiveParseError> {
            Ok(Self)
        }
        fn scope(&self) -> DirectiveScope {
            DirectiveScope::Standalone(StandaloneTarget::NextStatement)
        }
    }

    /// A directive that insists on one argument, to check the registry hands
    /// `parse` what it promised.
    #[derive(Debug)]
    struct NeedsName;

    impl Directive for NeedsName {
        fn key() -> DirectiveKey {
            DirectiveKey("needsName")
        }
        fn parse(args: &Args, span: &Span) -> Result<Self, DirectiveParseError> {
            args.get(0)
                .map(|_| Self)
                .ok_or_else(|| DirectiveParseError::UnexpectedArguments {
                    key: "needsName".to_string(),
                    span: span.clone(),
                })
        }
        fn scope(&self) -> DirectiveScope {
            DirectiveScope::Standalone(StandaloneTarget::NextStatement)
        }
    }

    fn raw(path: &[&'static str], args: Option<&'static str>) -> RawDirective<'static> {
        RawDirective {
            path: path.to_vec(),
            args,
            span: Span::in_memory(7, 3),
        }
    }

    // Traceability to `db/src/migrations/PLAN.md`, "Verification →
    // Directive/registry-level tests":
    //
    // | PLAN bullet | Test |
    // |---|---|
    // | An unregistered key (`up.bgin`) yields `UnknownDirective` with the key and span | `an_unregistered_key_reports_its_key_and_span` |
    // | `parse_args` cases (no args, positional, named, mixed, malformed) | `args::tests` — the section "the cases named in the plan" |
    // | Two directives under one key: rejected at registration, or documented last-write-wins — pick one and test | `a_duplicate_key_is_rejected_at_registration` (rejected) |
    #[test]
    fn registration_resolves_a_registered_key() {
        let mut registry = DirectiveRegistry::new();
        registry.register::<Alpha>().expect("first registration");
        let resolved = registry
            .resolve(&raw(&["alpha"], None))
            .expect("alpha is registered");
        assert_eq!(
            resolved.scope(),
            DirectiveScope::Standalone(StandaloneTarget::NextStatement)
        );
    }

    #[test]
    fn dotted_keys_register_and_resolve_as_written() {
        let mut registry = DirectiveRegistry::new();
        registry.register::<Alpha>().expect("ok");
        // `alpha` is registered; a dotted variant of it is a different key.
        let error = registry
            .resolve(&raw(&["alpha", "begin"], None))
            .expect_err("alpha.begin is not registered");
        match error {
            DirectiveParseError::UnknownDirective { key, span } => {
                assert_eq!(key, "alpha.begin");
                assert_eq!((span.line, span.col), (7, 3));
            }
            other => panic!("got {other:?}"),
        }
    }

    // The bullet: an unregistered key (e.g. a typo like `up.bgin`) produces
    // UnknownDirective with the offending key *and* span.
    #[test]
    fn an_unregistered_key_reports_its_key_and_span() {
        let registry = DirectiveRegistry::with_builtins().expect("built-ins register");
        let error = registry
            .resolve(&raw(&["up", "bgin"], None))
            .expect_err("up.bgin is not a directive");
        match error {
            DirectiveParseError::UnknownDirective { key, span } => {
                assert_eq!(key, "up.bgin");
                assert_eq!((span.line, span.col), (7, 3));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn an_empty_registry_knows_nothing() {
        let registry = DirectiveRegistry::new();
        assert!(registry.keys().is_empty());
        assert!(!registry.contains("up.begin"));
        assert!(
            registry
                .resolve(&raw(&["skipTx"], None))
                .err()
                .is_some_and(|e| matches!(e, DirectiveParseError::UnknownDirective { .. }))
        );
    }

    // The bullet: registering two directives under the same key is rejected at
    // registration time (chosen over last-write-wins).
    #[test]
    fn a_duplicate_key_is_rejected_at_registration() {
        #[derive(Debug)]
        struct AlphaCloned;
        impl Directive for AlphaCloned {
            fn key() -> DirectiveKey {
                DirectiveKey("alpha")
            }
            fn parse(_args: &Args, _span: &Span) -> Result<Self, DirectiveParseError> {
                Ok(Self)
            }
            fn scope(&self) -> DirectiveScope {
                DirectiveScope::Standalone(StandaloneTarget::NextStatement)
            }
        }

        let mut registry = DirectiveRegistry::new();
        registry.register::<Alpha>().expect("first wins");
        let error = registry
            .register::<AlphaCloned>()
            .expect_err("the second registration must be rejected");
        assert_eq!(error.key, "alpha");
        // The first registration still resolves.
        assert!(registry.resolve(&raw(&["alpha"], None)).is_ok());
    }

    #[test]
    fn distinct_keys_coexist() {
        let mut registry = DirectiveRegistry::new();
        registry.register::<Alpha>().expect("ok");
        registry.register::<Beta>().expect("ok");
        assert_eq!(registry.keys(), vec!["alpha", "beta"]);
    }

    #[test]
    fn the_registry_parses_arguments_before_the_directive_sees_them() {
        let mut registry = DirectiveRegistry::new();
        registry.register::<NeedsName>().expect("ok");
        // The argument reached the directive, so its own check is satisfied.
        assert!(
            registry
                .resolve(&raw(&["needsName"], Some("\"tool\"")))
                .is_ok()
        );
        // And with no argument, the directive's rejection surfaces.
        let error = registry
            .resolve(&raw(&["needsName"], None))
            .expect_err("the directive needs an argument");
        assert!(
            matches!(
                error,
                DirectiveParseError::UnexpectedArguments { ref key, .. } if key == "needsName"
            ),
            "got {error:?}"
        );
    }

    #[test]
    fn malformed_arguments_fail_resolution() {
        let mut registry = DirectiveRegistry::new();
        registry.register::<NeedsName>().expect("ok");
        let error = registry
            .resolve(&raw(&["needsName"], Some("\"unterminated")))
            .expect_err("unterminated string");
        assert!(
            matches!(
                error,
                DirectiveParseError::Arguments(crate::error::ArgError::UnterminatedString { .. })
            ),
            "got {error:?}"
        );
    }

    #[test]
    fn builtins_are_all_registered_and_listed() {
        let registry = DirectiveRegistry::with_builtins().expect("built-ins register");
        assert_eq!(
            registry.keys(),
            vec!["down.begin", "down.end", "skipTx", "up.begin", "up.end"]
        );
        for key in ["up.begin", "up.end", "down.begin", "down.end", "skipTx"] {
            assert!(registry.contains(key), "{key} missing");
        }
    }
}
