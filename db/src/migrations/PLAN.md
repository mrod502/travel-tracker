# Plan

## Changes

### Comment directives

the migrator accepts directives defined in special comments. These directives can define special behavior.

Directives take the form `--migrate:(some directive statement)`

#### Example - Migration Up/Down + Nested SkipTx

```sql
--the following line tells the parser that the following block is the 'up' segment of the migration
--migrate:up.begin
    CREATE TABLE users (
        id UUID PRIMARY KEY NOT NULL DEFAULT gen_random_uuid(),
        name TEXT NULL
    );
-- this tells the parser to run the next statement without a transaction
--migrate:skipTx
CREATE INDEX CONCURRENTLY idx_users_name on users(name);
-- the following line tells the parser this is the end of the migrate's 'up' segment
--migrate:up.end
--the following line tells the parser that the following block is the 'down' segment of the migration
--migrate:down.begin
DROP INDEX idx_users_name;
DROP TABLE users;
-- the following line tells the parser this is the end of the migrate's 'down' segment
--migrate:down.end
```

## Internal representation

### Data Structures

```rust
    // treat these as a guide more than a literal prescription of a struct type.

    pub enum StatementOption {
        OptSkipTx,
        // ...
    }

    // StatementGroup is what defines a group of statements that constitute a file
    pub struct StatementGroup {
        // contains a series of individual SQL statements to be run.
        pub statements: Vec<Statement>,
    }

    pub struct Migration {
        // `up` is the StatementGroup that defines the normal file direction, ie moves the state of the migrations forward. It is **required**.
        // If no `--migrate:up*` or `--migrate:down*` is defined inside a file, it is assumed the statement(s) belong to `up`.
        pub up: StatementGroup,
        // `down` is the StatementGroup that defines the inverse of `up`. It is **optional**
        // it is not validated that `down` literally define the inverse of `up`, it is enforced only by convention
        pub down: Option<StatementGroup>,
    }
```

> **Resolved (Q1):** `StatementGroupOption` has been dropped for v1. `skipTx` is a per-statement option only (`StatementOption::OptSkipTx`), attached to the *next* statement parsed. See "Resolved: Q1" below for rationale. If group-wide skipTx is wanted later, it should be an explicit argument on `begin` (e.g. `--migrate:up.begin(skipTx)`), not inferred from comment placement.

### Traits

Directives are split into two concerns: **scope** (how a directive attaches structurally — does it open/close a region, or fire once?) and **effect** (what it actually does to parser state). This lets `begin`/`end` directives be handled generically by the parser core, while directives like `skipTx` carry their own logic.

```rust
/// Uniquely identifies a directive. A newtype over &'static str (rather than
/// a closed enum) so directives can be registered without editing a central match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DirectiveKey(pub &'static str);

/// How a directive attaches to the surrounding SQL.
pub enum DirectiveScope {
    /// Opens/closes a region; subsequent statements belong to `family`
    /// until the matching End is seen. Handled entirely by the parser core —
    /// Begin/End directives don't need custom `apply` logic.
    Begin { family: GroupFamily },
    End { family: GroupFamily },
    /// Fires once, immediately mutating parse state via `apply`.
    Standalone(StandaloneTarget),
}

pub enum GroupFamily { Up, Down }

pub enum StandaloneTarget {
    /// Applies only to the next statement parsed. If a block-end directive
    /// or EOF is reached first with no statement in between, this is a
    /// parse error (`OrphanedStatementDirective`).
    NextStatement,
    /// Applies to every statement in the enclosing group. Must appear
    /// before any statement has been accumulated in the current group.
    /// (Not used by any directive in v1 — reserved for future use.)
    EnclosingGroup,
}
```

```rust
pub struct RawDirective<'a> {
    pub path: Vec<&'a str>,      // ["up","begin"] or ["skipTx"]
    pub args: Option<&'a str>,   // raw text inside parens, if any
    pub span: Span,
}

pub trait Directive: fmt::Debug {
    fn key() -> DirectiveKey where Self: Sized;

    fn parse(args: &Args, span: &Span) -> Result<Self, DirectiveParseError>
    where Self: Sized;

    /// A method, not an assoc fn, in case future arg-driven variants
    /// (e.g. `skipTx(scope="group")`) need to pick their scope dynamically.
    fn scope(&self) -> DirectiveScope;

    /// Default no-op: Begin/End directives never need this, since the
    /// parser core handles group push/pop generically off `scope()`.
    /// Standalone directives (skipTx, etc.) override it.
    fn apply(&self, ctx: &mut ParseContext) -> Result<(), DirectiveApplyError> {
        Ok(())
    }
}
```

`Directive::parse` returns `Self`, so the trait isn't object-safe on its own. A blanket-impl wrapper gives dynamic dispatch after parsing, for the registry:

```rust
pub trait DirectiveObj {
    fn scope(&self) -> DirectiveScope;
    fn apply(&self, ctx: &mut ParseContext) -> Result<(), DirectiveApplyError>;
}

impl<T: Directive> DirectiveObj for T {
    fn scope(&self) -> DirectiveScope { Directive::scope(self) }
    fn apply(&self, ctx: &mut ParseContext) -> Result<(), DirectiveApplyError> {
        Directive::apply(self, ctx)
    }
}

pub struct DirectiveRegistry {
    entries: HashMap<&'static str, Box<dyn Fn(&RawDirective) -> Result<Box<dyn DirectiveObj>, DirectiveParseError>>>,
}

impl DirectiveRegistry {
    pub fn register<D: Directive + 'static>(&mut self) {
        self.entries.insert(D::key().0, Box::new(|raw| {
            let args = parse_args(raw.args.unwrap_or(""))?;
            D::parse(&args, &raw.span).map(|d| Box::new(d) as Box<dyn DirectiveObj>)
        }));
    }

    pub fn resolve(&self, raw: &RawDirective) -> Result<Box<dyn DirectiveObj>, DirectiveParseError> {
        let key = raw.path.join(".");
        (self.entries.get(key.as_str())
            .ok_or_else(|| DirectiveParseError::UnknownDirective(key.clone()))?)(raw)
    }
}
```

`ParseContext` is the mutable state threaded through the whole file:

```rust
pub struct ParseContext {
    pub up: Option<StatementGroup>,
    pub down: Option<StatementGroup>,
    pub active: Option<GroupFamily>,       // None until first begin/statement
    pub current_statements: Vec<Statement>,
    pub pending_statement_options: Vec<StatementOption>, // cleared after each statement
    pub open_blocks: Vec<GroupFamily>,     // for validating begin/end nesting
}

impl ParseContext {
    fn begin_group(&mut self, family: GroupFamily) -> Result<(), DirectiveApplyError> { /* push, validate no duplicate open */ }
    fn end_group(&mut self, family: GroupFamily) -> Result<(), DirectiveApplyError> { /* pop, validate match, finalize into up/down */ }
    fn push_statement(&mut self, stmt: Statement) { /* attach pending options, clear them */ }
}
```

Driving loop:

```rust
pub trait MigrationParser {
    fn parse(&self, source: &str, registry: &DirectiveRegistry) -> Result<Migration, MigrationParseError>;
}

// core loop, roughly:
for token in lexer {
    match token {
        LexToken::Statement(stmt) => ctx.push_statement(stmt),
        LexToken::Directive(raw) => {
            let directive = registry.resolve(&raw)?;
            match directive.scope() {
                DirectiveScope::Begin { family } => ctx.begin_group(family)?,
                DirectiveScope::End { family }   => ctx.end_group(family)?,
                DirectiveScope::Standalone(_)    => directive.apply(&mut ctx)?,
            }
        }
    }
}
```

## Lexer / Statement splitting

The lexer has two jobs that have to happen in the same pass, because directive comments and statement boundaries both live inside the same character stream and both are sensitive to quoting: (1) split raw SQL text into individual `Statement`s on top-level `;`, and (2) recognize `--migrate:` comment lines and pull them out as directives rather than SQL text.

Doing this in two passes doesn't work: a naive "split on `;` first, find directives second" approach breaks on strings/dollar-quotes containing `;`, and a naive "find `--migrate:` lines first, split on `;` second" approach breaks if a directive comment is embedded inside a dollar-quoted function body. So there's a single character-level state machine.

### Lex state

```rust
pub enum LexState {
    Normal,
    LineComment,
    BlockComment { depth: u32 },      // Postgres allows nested block comments
    SingleQuoted,                     // '...' ; '' inside is an escaped quote
    DoubleQuoted,                     // "..." (quoted identifiers)
    DollarQuoted { tag: String },     // $$...$$ or $tag$...$tag$
}
```

Only `LexState::Normal` treats `;` as a statement terminator. Every other state exists specifically to make `;` (and `--`) inert while inside it.

### Directive vs. ordinary comment

Recognition happens only when entering `LineComment` state (i.e. right after `--` is seen in `Normal` state):

1. Peek the next bytes. If they are exactly `migrate:` (case-sensitive, **zero** whitespace between `--` and `migrate:`), this line is committed as a directive line, not SQL text.
2. Once committed, any whitespace appearing *before* the directive path (i.e. immediately after `migrate:`) is a hard `LexError::WhitespaceInDirective` — per Rule 1, `--migrate: up.begin` is an error, not a fallback to "ordinary comment."
3. If the peek doesn't match `migrate:` exactly (e.g. there's a space: `-- migrate:up.begin`, or a typo), it's an ordinary line comment: consumed and discarded like any other `--` comment, not included in the emitted `Statement` text.
4. The directive body runs to end-of-line. It's split on `(` into `path` (dot-separated, e.g. `["up","begin"]` or `["skipTx"]`) and an optional `args` slice (raw text between the parens, handed to `parse_args` later — see the trait section above).

This asymmetry — silently-ignored malformed prefix vs. hard error on malformed body — matches Rule 1's examples: `-- migrate:up.begin` (space after `--`) is *not* an error, it's just a comment; `--migrate: up.begin` (space after the colon, prefix already matched) *is* an error.

### Dollar-quote tag matching

`$$` and `$tag$` must be matched by their closing counterpart specifically (a `$$...$$` body can itself contain `$other$...$other$` in theory, though Postgres bodies rarely nest tags). On entering `DollarQuoted`, the lexer records the exact tag text (including empty string for `$$`) and only exits when it sees the same tag repeated, not just any `$...$`.

### Data structures produced

```rust
pub struct Span {
    pub file: PathBuf,
    pub line: usize,
    pub col: usize,
}

pub struct RawDirective<'a> {
    pub path: Vec<&'a str>,
    pub args: Option<&'a str>,
    pub span: Span,
}

pub struct Statement {
    pub sql: String,                     // trimmed statement text, semicolon excluded
    pub options: Vec<StatementOption>,    // populated later by ParseContext::push_statement
    pub span: Span,                       // start of statement, for error reporting
}

pub enum LexToken<'a> {
    Statement(Statement),
    Directive(RawDirective<'a>),
}
```

Note `Statement` is emitted by the lexer *without* `options` populated — the lexer only knows text and position, not directive semantics. `ParseContext::push_statement` is what attaches any `pending_statement_options` (e.g. from a preceding `skipTx`) before moving it into `current_statements`. This keeps the lexer directive-agnostic: it doesn't need to know what `skipTx` means, only that a `--migrate:` line isn't SQL text.

### Worked trace (using the example above)

| Input line | Lexer action |
|---|---|
| `--the following line tells...` | space after `--` → ordinary comment, discarded |
| `--migrate:up.begin` | committed directive → `Directive{ path: ["up","begin"] }` |
| `CREATE TABLE users (...);` | accumulated as SQL; `;` at `Normal` depth closes the statement → `Statement{ sql: "CREATE TABLE users (...)" }` |
| `-- this tells the parser...` | space after `--` → ordinary comment, discarded |
| `--migrate:skipTx` | committed directive → `Directive{ path: ["skipTx"] }` |
| `CREATE INDEX CONCURRENTLY idx_users_name on users(name);` | statement; note the `(name)` here is plain SQL, not directive args — the lexer is out of `LineComment`/directive context by this point, so parens are just parens |
| `--migrate:up.end` | committed directive → `Directive{ path: ["up","end"] }` |

### Open questions carried into this section

* **String escaping:** Postgres supports `E'...'` strings with backslash escapes when `standard_conforming_strings` is off (default on in modern Postgres, but configurable). v1 assumes standard-conforming strings (`''` is the only escape inside `'...'`) and treats `E'...'` the same as `'...'` for lexing purposes — worth flagging as a known gap rather than silently mis-lexing if someone's target DB has it off.
* **Nested block comments:** Postgres nests `/* /* */ */`; the `depth` field above handles this, but nested block comments containing a `--migrate:`-shaped line are never treated as directives (directives only trigger via `--` line comments in `Normal` state) — worth a one-line doc note so it's not surprising.
* **Directive inside a statement, mid-line:** current design assumes directives occupy their own line. Trailing directives on the same line as SQL (`SELECT 1; --migrate:skipTx`) aren't addressed yet — probably should be a parse error rather than silently accepted, but not yet decided.

## Implementation instructions

Target language is Rust. Treat every `struct`/`enum`/`trait` above as a starting point, not a locked contract — if you hit a spot where the sketch is ambiguous or wrong once you're implementing it, fix it and note the deviation in your PR description rather than working around it silently. Where this doc says "not yet decided" (the open-questions lists), make the smallest reasonable decision, document it inline as a `// DECISION:` comment at the point it matters, and keep it consistent — don't leave the behavior actually undefined in code.

### Suggested module layout

```
src/
  lex/
    mod.rs        // LexState machine, char-by-char scanner
    token.rs       // LexToken, RawDirective, Span
  directive/
    mod.rs        // Directive, DirectiveObj, DirectiveScope, DirectiveKey traits/enums
    registry.rs    // DirectiveRegistry
    args.rs        // Args, ArgValue, parse_args
    builtin/
      up_down.rs   // up.begin / up.end / down.begin / down.end
      skip_tx.rs   // skipTx
  parse/
    mod.rs         // MigrationParser, driving loop
    context.rs     // ParseContext
  model/
    mod.rs         // Statement, StatementOption, StatementGroup, Migration
  error.rs         // LexError, DirectiveParseError, DirectiveApplyError, MigrationParseError
```

Errors should carry `Span` wherever the underlying failure has a clear source location (all lex errors, all directive parse/apply errors) so a CLI layer can eventually print `file:line:col: message`. Don't implement the CLI itself as part of this task — only the library surface (`MigrationParser`, the model types, the registry) — unless told otherwise.

### Build order

Implement in this order; each stage should be independently testable before moving to the next, since later stages depend on earlier ones being correct rather than just "compiling."

1. **`Span`, `LexToken`, `RawDirective`** — pure data types, no logic.
2. **Lexer state machine** (`LexState` and the scanner around it). This is the highest-risk piece — get statement splitting and directive recognition correct on their own, including the quoting states, *before* anything about directive semantics exists. At this stage a directive is just an opaque `RawDirective { path, args, span }`; nothing validates that `up.begin` is a real directive yet.
3. **`parse_args`** — the function-call-argument mini-parser (`Args`/`ArgValue`), independent of any specific directive. Cover empty args, positional-only, named-only, mixed, and malformed input (unterminated string, trailing comma, unbalanced parens).
4. **`Directive` / `DirectiveObj` / `DirectiveScope` traits and `DirectiveRegistry`** — the generic machinery, with no built-in directives registered yet. Test registration/resolution/unknown-key behavior with a trivial fake directive.
5. **Built-in directives**: `up.begin`, `up.end`, `down.begin`, `down.end` (all `DirectiveScope::Begin`/`End`, no `apply` needed), then `skipTx` (`DirectiveScope::Standalone(NextStatement)`, with `apply` setting `pending_statement_options`).
6. **`ParseContext`** — `begin_group` / `end_group` / `push_statement`, including the nesting/duplicate-open/mismatched-close validation called out in the struct comments.
7. **`MigrationParser` driving loop** — wire lexer → registry → context together into the full `parse(source) -> Result<Migration, MigrationParseError>`.
8. Only after 1–7 are individually tested: wire up the "no `--migrate:up*`/`down*` at all → everything is `up`" fallback behavior described in `Migration`'s doc comment on `up`.

### Things to get right that are easy to get wrong

- `;` inside `'...'`, `"..."`, `$$...$$`/`$tag$...$tag$`, `-- ...`, and `/* ... */` must **not** split a statement. Get a failing test for each before trusting the state machine.
- `--migrate:` prefix match is byte-exact, case-sensitive, zero-tolerance on leading whitespace between `--` and `migrate:`. `-- migrate:...` (space present) is a normal comment, full stop — not a warning, not a degraded directive.
- Once the `--migrate:` prefix *has* matched, whitespace before the directive path is a hard `LexError`, not a silently-stripped normal comment. These two behaviors look similar but are opposite (ignore vs. error) — the dividing line is whether the exact `migrate:` prefix matched.
- Dollar-quote tags must match exactly on close (`$foo$` only closes on another literal `$foo$`, not `$$` or `$bar$`).
- `pending_statement_options` must be cleared after every statement, whether or not any option was pending, so a `skipTx` never silently "leaks" onto a second, unrelated statement.
- `begin_group`/`end_group` must reject: closing a family that was never opened, opening a family that's already open, and reaching EOF with a family still open. Reaching EOF with **no** family ever opened is *not* an error (implicit `up`, per the `Migration.up` doc comment) — don't conflate these two EOF cases.
- A file with zero `--migrate:` directives at all must produce a `Migration` with all statements in `up` and `down: None`.

## Verification

Write these as automated tests (unit tests per module, plus integration tests against whole-file fixtures) — don't rely on manual inspection. Use the exact worked example from this doc as your first integration fixture; it should round-trip to a `Migration` with `up` containing the two statements (the `CREATE TABLE` and the `CREATE INDEX CONCURRENTLY`, the latter carrying `StatementOption::OptSkipTx`) and `down` containing the two `DROP` statements, neither carrying `skipTx`.

### Lexer-level tests

- Statement splitting is unaffected by `;` inside: a single-quoted string, a double-quoted identifier, `$$...$$`, `$tag$...$tag$`, a line comment, a block comment, and a *nested* block comment.
- `''` inside a single-quoted string is one escaped quote, not a string terminator.
- A `--migrate:` line inside a dollar-quoted body (e.g. embedded in a `plpgsql` function) is treated as literal text, not extracted as a directive.
- `-- migrate:up.begin` (space present) lexes as an ordinary comment; no `Directive` token is emitted, and it does not appear in the resulting `Statement.sql`.
- `--migrate: up.begin` (space after colon) produces `LexError::WhitespaceInDirective`, not a silently-degraded comment.
- `migrate:left` (no leading `--` at all) is not directive-recognized at all — it's just SQL/text content, since directive recognition only triggers from inside a line comment.
- An unterminated string, unterminated dollar-quote, and unterminated block comment at EOF each produce a distinct, identifiable lex error rather than silently truncating.

### Directive/registry-level tests

- Resolving an unregistered key (e.g. a typo like `up.bgin`) produces `DirectiveParseError::UnknownDirective` with the offending key and span.
- `parse_args` correctly parses: no args, `("a")`, `(a="b")`, `("a", b="c")`, and rejects malformed input (unbalanced parens, unterminated string) with a distinct error per case.
- Registering two directives under the same key is either rejected at registration time or explicitly documented as last-write-wins — pick one and test it; don't leave it as accidental behavior.

### Parser/context-level tests

- `skipTx` immediately followed by a statement attaches `OptSkipTx` to exactly that one statement.
- `skipTx` with no following statement before a block-end directive or EOF produces `OrphanedStatementDirective`.
- `skipTx` appearing twice in a row before a single statement — decide and test the behavior (error, or last-one-wins) rather than leaving it unspecified.
- `up.begin` without a matching `up.end` before EOF is an error; likewise for `down`.
- `up.end` with no preceding `up.begin` is an error.
- `up.begin` appearing twice without an intervening `up.end` is an error (duplicate open).
- A file with only bare SQL statements and no directives at all → everything lands in `up`, `down` is `None`, no errors.
- A file with only `down.begin`/`down.end` and no `up` content at all — decide whether `up` (required per the `Migration` struct) being empty is an error, and test that decision explicitly, since the doc states `up` is required but doesn't currently say what "required" means when a `down`-only file is otherwise well-formed.

### Suggested tooling

- Standard `#[test]` unit tests colocated with each module for the lexer/args/registry-level cases.
- A `tests/fixtures/*.sql` + snapshot-style integration test (e.g. one fixture file per behavior above, asserting the resulting `Migration` or the specific error variant) so new fixtures are cheap to add as edge cases are discovered later.
- Property/fuzz testing on the lexer's quote-balancing (feed it randomly nested combinations of `'`, `"`, `$$`, `--`, `/*`, `*/`) is a good stretch goal but not required for initial acceptance.

## Acceptance criteria

Implementation is considered done when all of the following hold:

1. The worked example in this doc parses to the exact `Migration` structure described above, with no errors.
2. Every bullet under "Lexer-level tests," "Directive/registry-level tests," and "Parser/context-level tests" above exists as a passing automated test, each clearly traceable to the bullet it covers (test name or comment).
3. Rule 1 from this doc (`-- migrate:up.begin` is a normal comment; `--migrate: up.begin` is an error; unknown directives are errors) is enforced exactly as stated, with tests for all three cases.
4. None of the "easy to get wrong" items above are left unhandled — each has at least one corresponding failing-then-passing test.
5. Every item in this doc's three "Open Questions" sections (the original Q1–Q3 at the top, and the lexer open questions) is either resolved by a `// DECISION:` comment plus test, or explicitly left as a tracked TODO with a comment explaining why it's deferred and what the interim behavior is. None are silently unhandled.
6. Public API surface (`MigrationParser::parse`, `DirectiveRegistry::register`/`resolve`, the `model` types) has doc comments sufficient for a downstream caller — e.g. a future CLI — to use it without reading the implementation.
7. `cargo clippy` and `cargo fmt --check` are clean, and there is no `unwrap()`/`expect()`/`panic!` on any input-derived path (lexing, directive parsing, arg parsing, context transitions) — all such failures surface as typed `Result` errors with `Span`.
