//! Fixture-driven tests over whole migration files.
//!
//! One fixture per behaviour, asserting either the exact `Migration` it produces
//! or the exact error variant it must fail with. `every_fixture_is_asserted`
//! fails if a fixture is added to the directory without being covered below, so
//! new edge cases cannot quietly go untested.
//!
//! Traceability to `db/src/migrations/PLAN.md`:
//!
//! | Acceptance bullet | Test |
//! |---|---|
//! | Worked example round-trips | `the_worked_example_from_the_plan_round_trips` |
//! | Rule 1 (`-- migrate:` comment / `--migrate: ` error / unknown directive error) | `rule_one_is_enforced_exactly_as_stated` |
//! | No directives at all → everything in `up`, `down: None` | `a_file_with_no_directives_puts_everything_in_up` |
//! | `;` inert inside every quoting form | `semicolons_stay_inert_inside_quoted_regions` |
//! | `skipTx` binds to exactly one statement | `skip_tx_binds_to_the_statement_it_precedes` |
//! | `skipTx` orphaned before a block end / at EOF | `a_skip_tx_with_no_statement_before_a_block_end_is_an_error`, `a_skip_tx_at_end_of_file_is_an_error` |
//! | `up.begin` without `up.end` (and `down`) | `an_unclosed_up_block_is_an_error` |
//! | `up.end` without `up.begin` | `a_block_end_without_a_begin_is_an_error` |
//! | `up.begin` twice | `a_duplicate_open_is_an_error`, `a_group_cannot_be_defined_twice` |
//! | `down`-only file | `a_down_only_file_is_rejected` |
//! | Lexer unterminated regions | `unterminated_regions_are_distinct_lex_errors` |
//!
//! Committed-migration coverage, which is what the runner actually parses:
//!
//! | Claim | Test |
//! |---|---|
//! | Every committed file declares `up`, and a revert except the registry floor | `the_committed_migrations_declare_both_directions` |
//! | Folding each `.down.sql` into the file changed no SQL | `the_committed_corpus_still_means_the_same_thing` |
//! | A plpgsql body in a shipped file stays one statement | `a_dollar_quoted_body_stays_one_statement_in_the_corpus` |
//! | Syntax `sqlparser` could not parse is ordinary statement text | `postgres_only_constructs_are_ordinary_statement_text` |

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use db::error::{DirectiveApplyError, DirectiveParseError, LexError, MigrationParseError};
use db::model::StatementOption;
use db::{Migration, MigrationParser, StandardMigrationParser};

/// Every fixture the tests below assert on. Adding a file to the directory
/// without adding it here (and a test) fails `every_fixture_is_asserted`.
const ASSERTED_FIXTURES: &[&str] = &[
    "worked_example.sql",
    "no_directives.sql",
    "quoted_bodies.sql",
    "nested_block_comments.sql",
    "skip_tx_one_statement.sql",
    "prefix_must_be_exact.sql",
    "error_whitespace_after_colon.sql",
    "error_unknown_directive.sql",
    "error_unclosed_up.sql",
    "error_end_without_begin.sql",
    "error_mismatched_close.sql",
    "error_duplicate_up_begin.sql",
    "error_up_defined_twice.sql",
    "error_down_only.sql",
    "error_empty_down.sql",
    "error_skip_tx_before_block_end.sql",
    "error_skip_tx_at_eof.sql",
    "error_unterminated_string.sql",
    "error_unterminated_dollar_quote.sql",
    "error_unterminated_block_comment.sql",
    "error_empty_file.sql",
    "error_only_comments.sql",
];

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn registry() -> db::DirectiveRegistry {
    db::DirectiveRegistry::with_builtins().expect("built-in directive keys are distinct")
}

fn parse(name: &str) -> Result<Migration, MigrationParseError> {
    let path = fixtures_dir().join(name);
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture {name} should be readable: {e}"));
    StandardMigrationParser::new(&path).parse(&source, &registry())
}

fn parsed(name: &str) -> Migration {
    parse(name).unwrap_or_else(|e| panic!("{name} should parse: {e}"))
}

fn failed(name: &str) -> MigrationParseError {
    parse(name)
        .err()
        .unwrap_or_else(|| panic!("{name} should be rejected"))
}

/// Whitespace-normalised statement text: a dropped comment leaves one space
/// behind, so a run of them is expected and irrelevant to PostgreSQL.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sqls(migration: &Migration) -> Vec<String> {
    migration
        .up
        .statements
        .iter()
        .map(|s| collapse(&s.sql))
        .collect()
}

// ==== acceptance criterion 1: the worked example ===========================

#[test]
fn the_worked_example_from_the_plan_round_trips() {
    let migration = parsed("worked_example.sql");

    assert_eq!(
        migration.up.statements.len(),
        2,
        "up: CREATE TABLE then CREATE INDEX CONCURRENTLY"
    );
    assert_eq!(
        collapse(&migration.up.statements[0].sql),
        "CREATE TABLE users ( id UUID PRIMARY KEY NOT NULL DEFAULT gen_random_uuid(), name TEXT NULL )"
    );
    assert_eq!(
        collapse(&migration.up.statements[1].sql),
        "CREATE INDEX CONCURRENTLY idx_users_name on users(name)"
    );

    // The `skipTx` directive lands on the index it precedes and nowhere else.
    assert!(
        migration.up.statements[0].options.is_empty(),
        "CREATE TABLE runs inside the transaction"
    );
    assert_eq!(
        migration.up.statements[1].options,
        vec![StatementOption::OptSkipTx],
        "CREATE INDEX CONCURRENTLY is the statement marked to leave the transaction"
    );
    assert_eq!(migration.skip_tx_statements().len(), 1);

    let down = migration
        .down
        .clone()
        .expect("the file declares a down block");
    assert_eq!(down.len(), 2, "down: DROP INDEX then DROP TABLE");
    assert_eq!(
        collapse(&down.statements[0].sql),
        "DROP INDEX idx_users_name"
    );
    assert_eq!(collapse(&down.statements[1].sql), "DROP TABLE users");
    assert!(
        down.statements.iter().all(|s| s.options.is_empty()),
        "neither DROP carries skipTx"
    );

    // Positions line up with the file, which is what a CLI prints.
    assert_eq!(
        migration
            .up
            .statements
            .iter()
            .map(|s| (s.span.line, s.span.col))
            .collect::<Vec<_>>(),
        vec![(3, 5), (9, 1)],
        "the first statement starts at the `C` of CREATE, indented four columns"
    );
    assert_eq!(
        down.statements
            .iter()
            .map(|s| (s.span.line, s.span.col))
            .collect::<Vec<_>>(),
        vec![(14, 1), (15, 1)]
    );
    assert!(migration.is_revertible());
}

// ==== acceptance criterion 3: Rule 1 =======================================

#[test]
fn rule_one_is_enforced_exactly_as_stated() {
    // (a) `-- migrate:up.begin` is a normal comment: no directive, no error,
    // and nothing leaked into statement text.
    let migration = parsed("prefix_must_be_exact.sql");
    assert_eq!(
        sqls(&migration),
        vec![
            "SELECT migrate:left FROM nowhere".to_string(),
            "SELECT 2".to_string(),
        ],
        "the three look-alike comments are comments; only the exact prefix is a directive"
    );
    assert!(migration.down.is_none());

    // (b) `--migrate: up.begin` — prefix matched, so whitespace is an error.
    let error = failed("error_whitespace_after_colon.sql");
    assert!(
        matches!(
            error,
            MigrationParseError::Lex(LexError::WhitespaceInDirective { .. })
        ),
        "got {error:?}"
    );
    assert_eq!(
        error.span().map(|s| (s.line, s.col)),
        Some((1, 1)),
        "the error points at the `--` that opened the directive"
    );

    // (c) An unknown directive is an error, naming the key.
    let error = failed("error_unknown_directive.sql");
    match error {
        MigrationParseError::Directive(DirectiveParseError::UnknownDirective { key, span }) => {
            assert_eq!(key, "up.bgin");
            assert_eq!((span.line, span.col), (3, 1));
        }
        other => panic!("got {other:?}"),
    }
}

// ==== the implicit-`up` fallback ===========================================

#[test]
fn a_file_with_no_directives_puts_everything_in_up() {
    let migration = parsed("no_directives.sql");

    assert_eq!(
        sqls(&migration),
        vec![
            "CREATE TABLE gauges ( id UUID PRIMARY KEY NOT NULL DEFAULT gen_random_uuid(), reading NUMERIC NOT NULL, observed_at TIMESTAMPTZ NOT NULL )".to_string(),
            "CREATE INDEX idx_gauges_observed_at ON gauges (observed_at)".to_string(),
            "COMMENT ON TABLE gauges IS 'readings that were never a directive'".to_string(),
        ]
    );
    assert!(migration.down.is_none(), "no down was declared");
    assert!(
        migration
            .up
            .statements
            .iter()
            .all(|s| !s.sql.contains(';') && !s.sql.contains("semicolon")),
        "semicolons inside comments must neither split nor survive into statement text: {:?}",
        sqls(&migration)
    );
}

// ==== the lexer, end to end ================================================

#[test]
fn semicolons_stay_inert_inside_quoted_regions() {
    let migration = parsed("quoted_bodies.sql");
    let statements = sqls(&migration);

    assert_eq!(
        statements,
        vec![
            "SELECT 'a; b' AS quoted".to_string(),
            r#"SELECT "odd;identifier" FROM (SELECT 1 AS "odd;identifier") AS t"#.to_string(),
            "CREATE FUNCTION untaged_body() RETURNS text LANGUAGE sql AS $$ SELECT 'inner; semicolon'; $$".to_string(),
            "CREATE FUNCTION tagged_body() RETURNS integer LANGUAGE plpgsql AS $fn$ DECLARE total integer := 0; BEGIN --migrate:skipTx total := total + 1; RETURN total; END; $fn$".to_string(),
            "SELECT 'it''s; still one statement' AS escaped_quote".to_string(),
        ],
        "each quoting form keeps its semicolons, and the `--migrate:skipTx` inside the \
         `$fn$` body is body text rather than a directive"
    );
    assert!(
        migration.skip_tx_statements().is_empty(),
        "a directive-looking line inside a dollar-quoted body must not apply skipTx"
    );
}

#[test]
fn nested_block_comments_are_one_comment() {
    let migration = parsed("nested_block_comments.sql");
    assert_eq!(
        sqls(&migration),
        vec!["SELECT 1 AS nested".to_string()],
        "the inner `*/` must not close the outer comment"
    );
}

#[test]
fn skip_tx_binds_to_the_statement_it_precedes() {
    let migration = parsed("skip_tx_one_statement.sql");

    let skip: Vec<String> = migration
        .skip_tx_statements()
        .iter()
        .map(|s| collapse(&s.sql))
        .collect();
    assert_eq!(
        skip,
        vec!["CREATE INDEX CONCURRENTLY idx_events_name ON events (name)".to_string()]
    );

    let plain = migration
        .up
        .statements
        .iter()
        .filter(|s| !s.has_option(StatementOption::OptSkipTx))
        .map(|s| collapse(&s.sql))
        .collect::<Vec<_>>();
    assert_eq!(plain.len(), 2, "{plain:?}");
    assert!(plain[1].starts_with("CREATE INDEX idx_events_name_plain"));
}

// ==== structural errors ====================================================

#[test]
fn an_unclosed_up_block_is_an_error() {
    let error = failed("error_unclosed_up.sql");
    match error {
        MigrationParseError::Apply(DirectiveApplyError::UnclosedGroup { family, span }) => {
            assert_eq!(family.to_string(), "up");
            assert_eq!(span.line, 1);
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn a_block_end_without_a_begin_is_an_error() {
    let error = failed("error_end_without_begin.sql");
    match error {
        MigrationParseError::Apply(DirectiveApplyError::CloseWithoutOpen { family, span }) => {
            assert_eq!(family.to_string(), "down");
            assert_eq!(span.line, 2);
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn closing_a_different_group_than_the_one_open_is_an_error() {
    let error = failed("error_mismatched_close.sql");
    match error {
        MigrationParseError::Apply(DirectiveApplyError::MismatchedClose {
            expected,
            found,
            ..
        }) => {
            assert_eq!(expected.to_string(), "up");
            assert_eq!(found.to_string(), "down");
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn a_duplicate_open_is_an_error() {
    let error = failed("error_duplicate_up_begin.sql");
    assert!(
        matches!(
            error,
            MigrationParseError::Apply(DirectiveApplyError::DuplicateGroupOpen { .. })
        ),
        "got {error:?}"
    );
}

#[test]
fn a_group_cannot_be_defined_twice() {
    let error = failed("error_up_defined_twice.sql");
    assert!(
        matches!(
            error,
            MigrationParseError::Apply(DirectiveApplyError::GroupAlreadyDefined { .. })
        ),
        "got {error:?}"
    );
}

#[test]
fn a_down_only_file_is_rejected() {
    let error = failed("error_down_only.sql");
    assert!(
        matches!(error, MigrationParseError::EmptyUp { .. }),
        "`up` is required; got {error:?}"
    );
    let message = error.to_string();
    assert!(
        message.contains("no `up` statements"),
        "the message has to say what is missing: {message}"
    );
}

#[test]
fn a_declared_but_empty_down_block_is_rejected() {
    let error = failed("error_empty_down.sql");
    match error {
        MigrationParseError::EmptyDown { span } => assert_eq!(span.line, 6),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn a_skip_tx_with_no_statement_before_a_block_end_is_an_error() {
    let error = failed("error_skip_tx_before_block_end.sql");
    match error {
        MigrationParseError::Apply(DirectiveApplyError::OrphanedStatementDirective { span }) => {
            assert_eq!(
                span.line, 3,
                "the orphan is the skipTx line, not the up.end"
            );
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn a_skip_tx_at_end_of_file_is_an_error() {
    let error = failed("error_skip_tx_at_eof.sql");
    assert!(
        matches!(
            error,
            MigrationParseError::Apply(DirectiveApplyError::OrphanedStatementDirective { .. })
        ),
        "got {error:?}"
    );
}

#[test]
fn unterminated_regions_are_distinct_lex_errors() {
    /// `(fixture, is this the error expected here, what to call it when failing)`
    type Case = (&'static str, fn(&LexError) -> bool, &'static str);

    let cases: [Case; 3] = [
        (
            "error_unterminated_string.sql",
            |e| matches!(e, LexError::UnterminatedString { .. }),
            "string literal",
        ),
        (
            "error_unterminated_dollar_quote.sql",
            |e| matches!(e, LexError::UnterminatedDollarQuote { .. }),
            "dollar quote",
        ),
        (
            "error_unterminated_block_comment.sql",
            |e| matches!(e, LexError::UnterminatedBlockComment { .. }),
            "block comment",
        ),
    ];

    for (fixture, is_expected, what) in cases {
        let error = failed(fixture);
        let MigrationParseError::Lex(lex) = error else {
            panic!("{fixture} should be a lex error, got {error:?}");
        };
        assert!(is_expected(&lex), "{what}: got {lex:?}");
        assert!(
            lex.span().line >= 1,
            "{what} errors carry a position: {lex}"
        );
        // The message names the location, which is what a CLI prints.
        assert!(
            lex.to_string().contains(':'),
            "{what} message should be `file:line:col: …`: {lex}"
        );
    }
}

#[test]
fn an_unterminated_dollar_quote_names_the_tag_it_was_waiting_for() {
    let error = failed("error_unterminated_dollar_quote.sql");
    match error {
        MigrationParseError::Lex(LexError::UnterminatedDollarQuote { tag, span }) => {
            assert_eq!(tag, "$fn$");
            assert_eq!(span.line, 2);
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn files_with_no_statements_at_all_are_rejected() {
    for fixture in ["error_empty_file.sql", "error_only_comments.sql"] {
        let error = failed(fixture);
        assert!(
            matches!(error, MigrationParseError::EmptyUp { .. }),
            "{fixture} should be rejected as an empty up, got {error:?}"
        );
    }
}

// ==== the corpus that already exists =======================================

/// The migration that creates the `migrations` table: the floor of `db down`,
/// and so the only committed file with no revert of its own.
const REGISTRY_FILE: &str = "202607312100_create_migrations_registry.sql";

/// Every committed migration file, oldest first.
fn committed_files() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/migrations");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{} should be readable: {e}", dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .collect();
    files.sort();
    files
}

/// Parse a committed migration by file name.
fn committed(name: &str) -> Migration {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/migrations")
        .join(name);
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} should be readable: {e}", path.display()));
    StandardMigrationParser::new(&path)
        .parse(&source, &registry())
        .unwrap_or_else(|e| panic!("{} should parse: {e}", path.display()))
}

/// FNV-1a over the collapsed statement text, with the statement count in front:
/// a change detector for a committed migration, and nothing beyond that.
fn digest(statements: &[db::Statement]) -> String {
    let joined = statements
        .iter()
        .map(|s| collapse(&s.sql))
        .collect::<Vec<_>>()
        .join("\n");

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in joined.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{}/{hash:016x}", statements.len())
}

/// `0/0…0` for a file that declares no revert, so "no revert" and "a revert that
/// undoes nothing" can never share a value.
fn digest_or_none(down: Option<&db::StatementGroup>) -> String {
    down.map(|group| digest(&group.statements))
        .unwrap_or_else(|| String::from("0/0000000000000000"))
}

/// A statement's invariants, applied to both directions: never empty, never
/// carrying its own terminator, and never splitting when parsed again on its
/// own. That last one is the real test — a `;` inside a string literal, a
/// dollar-quoted body, or a `COMMENT ON … IS '…'` text has to stay inside the
/// statement it was lexed with.
fn assert_statement_is_whole(where_: &str, statement: &db::Statement) {
    assert!(
        !statement.sql.trim().is_empty(),
        "{where_}: no empty statements: {statement:?}"
    );
    assert!(
        !statement.sql.ends_with(';'),
        "{where_}: the terminator is not part of statement text: {:?}",
        statement.sql
    );

    let reparsed = StandardMigrationParser::in_memory()
        .parse(&statement.sql, &registry())
        .unwrap_or_else(|e| panic!("{where_}: statement should re-parse: {e}"));
    assert_eq!(
        reparsed.up.statements.len(),
        1,
        "{where_}: statement split on re-parse: {:?}",
        statement.sql
    );
}

/// Every committed migration is written in the directive format: an explicit
/// `up` block and — except the registry, which is `db down`'s floor — a `down`
/// block that reverts it. None of them asks to leave the transaction.
#[test]
fn the_committed_migrations_declare_both_directions() {
    let files = committed_files();
    assert!(
        files.len() > 5,
        "expected the committed migration set, found {} files",
        files.len()
    );

    for path in files {
        let name = path
            .file_name()
            .expect("file has a name")
            .to_string_lossy()
            .into_owned();
        let migration = committed(&name);

        assert!(!migration.up.is_empty(), "{name} has statements in up");

        if name == REGISTRY_FILE {
            assert!(
                migration.down.is_none(),
                "{name} is the floor of db down; declaring a revert would let it erase \
                 the history it reads"
            );
        } else {
            let Some(down) = migration.down.as_ref() else {
                panic!("{name} declares no --migrate:down block, so db down cannot revert it");
            };
            assert!(
                !down.is_empty(),
                "{name}'s revert block has statements (the parser rejects an empty one)"
            );
        }

        assert!(
            migration.skip_tx_statements().is_empty(),
            "{name} uses no skipTx directive"
        );

        let both = migration
            .up
            .statements
            .iter()
            .chain(migration.down.iter().flat_map(|group| &group.statements));
        for statement in both {
            assert_statement_is_whole(&name, statement);
        }
    }
}

/// The claim that made it safe to fold each `<name>.down.sql` into a
/// `--migrate:down` block: not one statement of SQL changed. Kept as a test
/// because a committed migration has already been applied somewhere, and those
/// databases cannot be diffed afterwards — this is the only place a change to
/// what a shipped migration *does* becomes visible.
#[test]
fn the_committed_corpus_still_means_the_same_thing() {
    let mut rows = String::new();
    for path in committed_files() {
        let name = path
            .file_name()
            .expect("file has a name")
            .to_string_lossy()
            .into_owned();
        let migration = committed(&name);
        rows.push_str(&format!(
            "{name}\tup={}\tdown={}\n",
            digest(&migration.up.statements),
            digest_or_none(migration.down.as_ref())
        ));
    }

    let fixture = fixtures_dir().join("shipped_corpus.txt");
    if std::env::var_os("DB_CORPUS_REGEN").is_some() {
        let existing = std::fs::read_to_string(&fixture).unwrap_or_default();
        let header: String = existing
            .lines()
            .take_while(|line| line.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        let header = if header.is_empty() {
            String::from("# Regenerated corpus digest; see the test that writes this.\n")
        } else {
            format!("{header}\n")
        };
        std::fs::write(&fixture, format!("{header}{rows}"))
            .expect("corpus fixture should be writable");
        return;
    }

    let text = std::fs::read_to_string(&fixture).expect("corpus fixture should be readable");
    let expected: Vec<&str> = text
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .collect();
    let actual: Vec<&str> = rows
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();

    assert_eq!(
        expected.len(),
        actual.len(),
        "the committed corpus has {} files, the fixture records {}\nactual:\n{rows}",
        actual.len(),
        expected.len()
    );

    let changed: Vec<String> = expected
        .iter()
        .zip(&actual)
        .filter(|(want, got)| want != got)
        .map(|(want, got)| format!("  recorded {want}\n  now     {got}"))
        .collect();
    assert!(
        changed.is_empty(),
        "statements changed in committed migrations:\n{}\nIf that is intended, say so in the \
         commit message and regenerate: DB_CORPUS_REGEN=1 cargo test -p db --test \
         parse_migration_file",
        changed.join("\n")
    );
}

/// The one committed file whose statements the old splitter could only handle by
/// giving up on `sqlparser` and scanning: a plpgsql body full of semicolons.
/// Against the real file, the body has to be one statement — that is the bug
/// that used to surface as "unterminated dollar-quoted string" at apply time.
#[test]
fn a_dollar_quoted_body_stays_one_statement_in_the_corpus() {
    let migration = committed("202609021200_partition_occurrences_forward.sql");

    assert_eq!(migration.up.len(), 3, "{:#?}", migration.up);
    let function = &migration.up.statements[0];
    assert!(
        function
            .sql
            .contains("CREATE OR REPLACE FUNCTION ensure_occurrence_partitions"),
        "{}",
        collapse(&function.sql)
    );
    assert!(
        function.sql.contains("date_trunc('month', now())::date;"),
        "a semicolon inside the body must not have split it: {}",
        collapse(&function.sql)
    );
    assert!(
        function.sql.trim_end().ends_with("$fn$"),
        "the body must end where its tag closes: {}",
        collapse(&function.sql)
    );
    assert!(
        migration.up.statements[1]
            .sql
            .trim_start()
            .starts_with("COMMENT ON FUNCTION"),
        "{}",
        collapse(&migration.up.statements[1].sql)
    );
    assert!(
        migration.up.statements[2]
            .sql
            .contains("SELECT ensure_occurrence_partitions(15)"),
        "{}",
        collapse(&migration.up.statements[2].sql)
    );
}

/// The PostgreSQL constructs that used to send the runner's splitter into its
/// fallback — and, before there was a fallback, made it cut statements in the
/// wrong place — are ordinary text to a lexer that knows no dialect. These are
/// the inputs the deleted `split_query` tests used to assert on; they belong
/// with the parser now.
#[test]
fn postgres_only_constructs_are_ordinary_statement_text() {
    let source = r#"
CREATE TABLE test_table (
    id UUID NOT NULL DEFAULT uuidv7(),
    data JSONB NOT NULL DEFAULT '{}',
    computed_col TEXT GENERATED ALWAYS AS (upper(data::text)) STORED
);
CREATE INDEX idx_test ON test_table USING GIN (data);
INSERT INTO test_table (data) VALUES ('{"key": "value"}');
SELECT * FROM h3_to_string(123);
DROP EXTENSION IF EXISTS h3 RESTRICT;
CREATE TABLE occurrences (
    occurrence_id UUID NOT NULL DEFAULT uuidv7(),
    location GEOGRAPHY(POINT, 4326),
    geo_cell H3INDEX GENERATED ALWAYS AS (h3_latlng_to_cell(ST_Force2D(location::geometry), 9)) STORED
);
"#;

    let migration = StandardMigrationParser::in_memory()
        .parse(source, &registry())
        .expect("dialect-specific syntax is just statement text");
    let statements: Vec<String> = migration
        .up
        .statements
        .iter()
        .map(|s| collapse(&s.sql))
        .collect();

    assert_eq!(statements.len(), 6, "{statements:#?}");
    for statement in &migration.up.statements {
        assert_statement_is_whole("postgres constructs", statement);
    }
    assert!(
        statements[0].contains("GENERATED ALWAYS AS (upper(data::text)) STORED"),
        "{}",
        statements[0]
    );
    assert!(
        statements[2].contains(r#"('{"key": "value"}')"#),
        "a JSON literal keeps its braces and colons: {}",
        statements[2]
    );
    assert!(
        statements[4].starts_with("DROP EXTENSION IF EXISTS h3 RESTRICT"),
        "the statement sqlparser 0.51 could not parse: {}",
        statements[4]
    );
    assert!(
        statements[5].contains("h3_latlng_to_cell(ST_Force2D(location::geometry), 9)"),
        "{}",
        statements[5]
    );
}

// ==== fixture bookkeeping ==================================================

#[test]
fn every_fixture_is_asserted() {
    let on_disk: BTreeSet<String> = std::fs::read_dir(fixtures_dir())
        .expect("fixtures directory should be readable")
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            path.extension().is_some_and(|ext| ext == "sql").then(|| {
                path.file_name()
                    .expect("entry has a name")
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect();
    let asserted: BTreeSet<String> = ASSERTED_FIXTURES
        .iter()
        .map(|name| (*name).to_string())
        .collect();

    let unasserted: Vec<&String> = on_disk.difference(&asserted).collect();
    assert!(
        unasserted.is_empty(),
        "fixtures with no test: {unasserted:?} — add an assertion for each new fixture"
    );
    let missing: Vec<&String> = asserted.difference(&on_disk).collect();
    assert!(
        missing.is_empty(),
        "tests name fixtures that no longer exist: {missing:?}",
    );
}
