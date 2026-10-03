//! Integration tests for the migration system
//!
//! These tests verify:
//! 1. FileAttrs ordering behavior
//! 2. Migration statement splitting and filtering
//! 3. Filename parsing edge cases
//!
//! Note: Database-dependent tests are skipped in CI/CD environments.
//! Use the unit tests in src/up.rs for database-agnostic validation.

use chrono::{Local, TimeZone};
use std::path::PathBuf;

// Copy FileAttrs from the source for testing
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileAttrs {
    name: String,
    full_path: PathBuf,
    created_at: chrono::DateTime<Local>,
}

impl Ord for FileAttrs {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.created_at
            .cmp(&other.created_at)
            .then_with(|| self.full_path.cmp(&other.full_path))
            .then_with(|| self.name.cmp(&other.name))
    }
}

impl PartialOrd for FileAttrs {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// ============================================================================
// FileAttrs Ordering Tests
// ============================================================================

#[test]
fn test_file_attrs_orders_by_timestamp() {
    let ts1 = Local.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let ts2 = Local.with_ymd_and_hms(2026, 12, 31, 23, 59, 59).unwrap();

    let early = FileAttrs {
        name: "early".to_string(),
        full_path: PathBuf::from("early.sql"),
        created_at: ts1,
    };

    let late = FileAttrs {
        name: "late".to_string(),
        full_path: PathBuf::from("late.sql"),
        created_at: ts2,
    };

    assert!(early < late, "Earlier migration should sort first");
    assert!(late > early, "Later migration should sort after");
}

#[test]
fn test_file_attrs_orders_by_path_when_timestamps_equal() {
    let ts = Local.with_ymd_and_hms(2026, 6, 15, 12, 0, 0).unwrap();

    let path_a = FileAttrs {
        name: "a".to_string(),
        full_path: PathBuf::from("01_a.sql"),
        created_at: ts,
    };

    let path_b = FileAttrs {
        name: "b".to_string(),
        full_path: PathBuf::from("02_b.sql"),
        created_at: ts,
    };

    assert!(
        path_a < path_b,
        "Same timestamp: earlier path should sort first"
    );
}

#[test]
fn test_file_attrs_orders_by_name_when_everything_else_equal() {
    let ts = Local.with_ymd_and_hms(2026, 6, 15, 12, 0, 0).unwrap();

    let name_a = FileAttrs {
        name: "aaa".to_string(),
        full_path: PathBuf::from("same.sql"),
        created_at: ts,
    };

    let name_b = FileAttrs {
        name: "bbb".to_string(),
        full_path: PathBuf::from("same.sql"),
        created_at: ts,
    };

    assert!(
        name_a < name_b,
        "Same timestamp and path: earlier name should sort first"
    );
}

#[test]
fn test_file_attrs_reflexivity() {
    let ts = Local.with_ymd_and_hms(2026, 6, 15, 12, 0, 0).unwrap();

    let attrs = FileAttrs {
        name: "test".to_string(),
        full_path: PathBuf::from("test.sql"),
        created_at: ts,
    };

    assert_eq!(attrs, attrs, "FileAttrs should be reflexive");
}

// ============================================================================
// Migration Statement Splitting Tests
// ============================================================================

// The runner splits with `db::Lexer`; these exercise it from outside the crate,
// the way `db` itself does. There is no longer a `sqlparser` path to fall back
// to, so dialect-specific syntax and a positional `$1` are just characters.
fn lex(source: &str) -> Vec<String> {
    db::Lexer::new(source, "integration_test.sql")
        .filter_map(|token| token.ok())
        .filter_map(|token| match token {
            db::LexToken::Statement(statement) => Some(statement.sql),
            db::LexToken::Directive(_) => None,
        })
        .collect()
}

#[test]
fn test_split_query_filters_empty_statements() {
    let statements =
        lex("CREATE TABLE a (id INT); CREATE INDEX idx ON a(id); INSERT INTO a VALUES (1);");

    assert_eq!(statements.len(), 3);
    assert!(statements[0].contains("CREATE TABLE"));
    assert!(statements[1].contains("CREATE INDEX"));
    assert!(statements[2].contains("INSERT"));
}

#[test]
fn test_split_query_handles_consecutive_semicolons() {
    assert_eq!(lex("SELECT 1;; SELECT 2;"), ["SELECT 1", "SELECT 2"]);
}

#[test]
fn test_split_query_trims_whitespace() {
    assert_eq!(lex("  SELECT 1;  SELECT 2;  "), ["SELECT 1", "SELECT 2"]);
}

// ============================================================================
// Filename Parsing Tests
// ============================================================================

/// Simulate the parse_file_name logic from UpArgs
// Note: MIGRATION_TIME_FMT = "%Y%m%d%H%M" (12 digits, no seconds)
fn parse_timestamp(timestamp_str: &str) -> Option<chrono::DateTime<Local>> {
    chrono::NaiveDateTime::parse_from_str(timestamp_str, "%Y%m%d%H%M")
        .ok()
        .map(|ndt| Local.from_utc_datetime(&ndt))
}

#[test]
fn test_parse_timestamp_valid() {
    let result = parse_timestamp("202603081008");
    assert!(result.is_some());
    let expected = Local.from_utc_datetime(
        &chrono::NaiveDateTime::parse_from_str("202603081008", "%Y%m%d%H%M").unwrap(),
    );
    assert_eq!(result.unwrap(), expected);
}

#[test]
fn test_parse_timestamp_full_name() {
    let full_name = "202607312147_create_bluetooth_occurrences.sql";
    let (ts_part, rest) = full_name.split_once('_').unwrap();
    let (name, ext) = rest.split_once('.').unwrap();

    assert_eq!(name, "create_bluetooth_occurrences");
    assert_eq!(ext, "sql");

    let ts = parse_timestamp(ts_part);
    assert!(ts.is_some());
}

#[test]
fn test_parse_timestamp_invalid() {
    assert!(parse_timestamp("invalid").is_none());
}

#[test]
fn test_parse_timestamp_too_short() {
    // 8 digits instead of 12
    assert!(parse_timestamp("20260308").is_none());
}

#[test]
fn test_parse_timestamp_too_long() {
    // 14 digits instead of 12
    assert!(parse_timestamp("20260308100812").is_none());
}

// ============================================================================
// End-to-End Migration Flow Tests (Simulated)
// ============================================================================

#[test]
fn test_migration_order_simulation() {
    // Simulate discovering and ordering multiple migrations
    let base_ts = Local.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

    let migrations = vec![
        FileAttrs {
            name: "z_last".to_string(),
            full_path: PathBuf::from("202601010001_z_last.sql"),
            created_at: base_ts,
        },
        FileAttrs {
            name: "a_first".to_string(),
            full_path: PathBuf::from("202601010000_a_first.sql"),
            created_at: Local.with_ymd_and_hms(2025, 12, 31, 23, 59, 59).unwrap(),
        },
        FileAttrs {
            name: "m_middle".to_string(),
            full_path: PathBuf::from("202601020000_m_middle.sql"),
            created_at: base_ts + chrono::Duration::days(1),
        },
    ];

    let mut sorted = migrations;
    sorted.sort();

    // Should be ordered by timestamp first
    assert_eq!(sorted[0].name, "a_first");
    assert_eq!(sorted[1].name, "z_last");
    assert_eq!(sorted[2].name, "m_middle");
}

#[test]
fn test_migration_filtering_by_latest() {
    // Simulate filtering migrations newer than the latest applied
    let base_ts = Local.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let latest_applied = Local.with_ymd_and_hms(2025, 12, 31, 23, 59, 59).unwrap();

    let all_migrations = vec![
        FileAttrs {
            name: "old".to_string(),
            full_path: PathBuf::from("202512300000_old.sql"),
            created_at: latest_applied - chrono::Duration::hours(1),
        },
        FileAttrs {
            name: "new".to_string(),
            full_path: PathBuf::from("202601020000_new.sql"),
            created_at: base_ts + chrono::Duration::days(1),
        },
    ];

    let to_run: Vec<_> = all_migrations
        .into_iter()
        .filter(|m| m.created_at > latest_applied)
        .collect();

    assert_eq!(to_run.len(), 1);
    assert_eq!(to_run[0].name, "new");
}
