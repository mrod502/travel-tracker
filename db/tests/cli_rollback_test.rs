//! End-to-end tests for `db up` / `db down`, driven through the real CLI.
//!
//! These need a PostgreSQL server, so they are skipped (loudly) when
//! `DATABASE_URL` is unset. Each test works in a database it creates and drops,
//! so it never touches the schema anybody is developing against.
//!
//! What they pin down:
//! * the registry is created by the migration that owns it, and is the floor of
//!   `db down` (M3);
//! * `--number N` means N, in both directions (M2);
//! * reverting a migration changes the schema, instead of `down` wiping it
//!   wholesale and re-applying everything (M1).

use sqlx::{AssertSqlSafe, Executor, PgPool, Row};
use std::path::Path;
use std::process::Command;

const DB_BIN: &str = env!("CARGO_BIN_EXE_db");
const MIGRATIONS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/migrations");

/// Connection parts for the scratch server, or `None` when the environment has
/// no database to test against.
struct Target {
    admin_url: String,
    host: String,
    port: u16,
    user: String,
    password: String,
}

impl Target {
    /// `postgres://user:password@host:port/database`
    fn from_env() -> Option<Target> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let rest = url
            .strip_prefix("postgres://")
            .or(url.strip_prefix("postgresql://"))?;
        let (credentials, location) = rest.rsplit_once('@')?;
        let (user, password) = credentials.split_once(':')?;
        let (host_port, _database) = location.split_once('/')?;
        let (host, port) = match host_port.split_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().unwrap_or(5432)),
            None => (host_port.to_string(), 5432),
        };
        Some(Target {
            admin_url: url.to_string(),
            host,
            port,
            user: user.to_string(),
            password: password.to_string(),
        })
    }

    fn is_local(&self) -> bool {
        self.host == "localhost" || self.host == "::1" || self.host.starts_with("127.")
    }

    /// A database this test owns, created from the admin URL.
    async fn create(&self, name: &str) -> PgPool {
        let admin = PgPool::connect(&self.admin_url)
            .await
            .expect("admin connection should work");
        admin
            .execute(AssertSqlSafe(format!("DROP DATABASE IF EXISTS {name}")))
            .await
            .expect("should be able to drop a stale scratch database");
        admin
            .execute(AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .await
            .expect("should be able to create a scratch database");
        drop(admin);

        PgPool::connect(&self.url_for(name))
            .await
            .expect("scratch database should accept connections")
    }

    fn url_for(&self, name: &str) -> String {
        format!(
            "postgres://{}:{}@{}:{}/{}",
            self.user, self.password, self.host, self.port, name
        )
    }

    /// Release every connection before dropping the database, and be loud if it
    /// survives: `DROP DATABASE` fails while sessions are still attached, and a
    /// leak nobody notices is a leak that outlives CI.
    async fn cleanup(&self, name: &str, pool: PgPool) {
        pool.close().await;
        let admin = PgPool::connect(&self.admin_url)
            .await
            .expect("admin connection should work");
        admin
            .execute(AssertSqlSafe(format!("DROP DATABASE IF EXISTS {name}")))
            .await
            .unwrap_or_else(|e| panic!("scratch database {name} was left behind: {e}"));
    }

    /// Run the CLI against `database`, returning (success, combined output).
    fn run(&self, database: &str, args: &[&str]) -> (bool, String) {
        let port = self.port.to_string();
        let output = Command::new(DB_BIN)
            .args([
                "--host",
                &self.host,
                "--port",
                &port,
                "--user",
                &self.user,
                "--db",
                database,
                "--log-level",
                "error",
            ])
            .args(args)
            .env("DB_PASSWORD", &self.password)
            // --migrations-path is last: clap takes the last value for a
            // repeated flag, and the subcommand needs it.
            .args(["--migrations-path", MIGRATIONS])
            .output()
            .expect("the db binary should be runnable");
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        (output.status.success(), text)
    }
}

async fn registry_rows(pool: &PgPool) -> Vec<String> {
    let rows = sqlx::query("SELECT name FROM migrations ORDER BY created_at, name")
        .fetch_all(pool)
        .await
        .expect("registry should be readable");
    rows.iter().map(|r| r.get::<String, _>("name")).collect()
}

/// The `H3INDEX` generated columns' expressions, so a test can prove the schema
/// itself moved rather than just a row being deleted.
async fn geo_cell_fine_expression(pool: &PgPool) -> Option<String> {
    sqlx::query_scalar(
        "SELECT pg_get_expr(d.adbin, d.adrelid)
           FROM pg_attribute a
           JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
          WHERE a.attrelid = 'occurrences'::regclass
            AND a.attname = 'geo_cell_fine'",
    )
    .fetch_optional(pool)
    .await
    .unwrap_or_default()
}

fn scratch_name(test: &str) -> String {
    format!("db_cli_{test}_{}", std::process::id())
}

#[tokio::test]
async fn down_reverts_one_migration_at_a_time_and_up_reapplies_it() {
    let Some(target) = Target::from_env() else {
        eprintln!("SKIP: DATABASE_URL is unset, cannot exercise db up/db down");
        return;
    };
    let db = scratch_name("roundtrip");
    let pool = target.create(&db).await;

    // --- up: every migration applies, registry is complete ------------------
    let (ok, out) = target.run(&db, &["up"]);
    assert!(ok, "db up failed: {out}");

    let applied = registry_rows(&pool).await;
    assert!(
        applied.contains(&"create_migrations_registry".to_string()),
        "the registry should be recorded as a migration of its own, got {applied:?}"
    );
    assert!(
        applied.len() > 5,
        "expected the whole migration set, got {applied:?}"
    );
    assert!(
        applied.first().map(String::as_str) == Some("create_migrations_registry"),
        "the registry migration must sort first, got {applied:?}"
    );

    let correct = "st_x";
    let expression = geo_cell_fine_expression(&pool).await.unwrap_or_default();
    assert!(
        expression.contains(correct),
        "geo_cell_fine should read longitude first, got {expression}"
    );

    // --- down --number 1: exactly one migration, and it is the newest --------
    let newest = applied
        .last()
        .expect("the registry cannot be empty after `up`")
        .clone();

    let (ok, out) = target.run(&db, &["down", "--number", "1", "--allow-destructive"]);
    assert!(ok, "db down --number 1 failed: {out}");

    let after_revert = registry_rows(&pool).await;
    assert_eq!(
        after_revert.len(),
        applied.len() - 1,
        "`down --number 1` must revert exactly one migration"
    );
    assert!(
        !after_revert.contains(&newest),
        "`{newest}` was the newest migration and should have been reverted: {after_revert:?}"
    );

    // --- down as far as the migration this test can see in the schema --------
    //
    // `fix_geo_cell_coordinate_order` is the revert with an effect readable from
    // `pg_get_expr`, which is why it is the probe. It is not necessarily the
    // newest migration, and treating it as one broke the day a migration was
    // added after it, so how far down it sits is computed, not assumed.
    let probe = "fix_geo_cell_coordinate_order";
    let steps_to_probe = applied.len()
        - applied
            .iter()
            .position(|name| name == probe)
            .unwrap_or_else(|| panic!("{probe} should be in the applied set: {applied:?}"));

    // One step is already spent above.
    let further = steps_to_probe - 1;
    if further > 0 {
        let steps = further.to_string();
        let (ok, out) = target.run(&db, &["down", "--number", &steps, "--allow-destructive"]);
        assert!(ok, "db down --number {steps} failed: {out}");
    }

    let after_revert = registry_rows(&pool).await;
    assert!(
        !after_revert.contains(&probe.to_string()),
        "{probe} should be gone after {steps_to_probe} steps down: {after_revert:?}"
    );
    let reverted = geo_cell_fine_expression(&pool).await.unwrap_or_default();
    assert!(
        reverted.contains("st_y(("),
        "reverting the coordinate-order fix must put the transposed expression \
         back in the schema, got {reverted}"
    );

    // --- up --number N: exactly those migrations back ------------------------
    let steps = steps_to_probe.to_string();
    let (ok, out) = target.run(&db, &["up", "--number", &steps]);
    assert!(ok, "db up --number {steps} failed: {out}");

    let after_reapply = registry_rows(&pool).await;
    assert_eq!(
        after_reapply, applied,
        "`up --number {steps}` must restore exactly the migrations that were reverted"
    );
    let restored = geo_cell_fine_expression(&pool).await.unwrap_or_default();
    assert_eq!(
        restored, expression,
        "schema should match the pre-revert one"
    );

    // --- down --number 0: everything down to the registry floor --------------
    let (ok, out) = target.run(&db, &["down", "--number", "0", "--allow-destructive"]);
    assert!(ok, "db down --number 0 failed: {out}");

    assert_eq!(
        registry_rows(&pool).await,
        vec!["create_migrations_registry".to_string()],
        "the registry is the floor: it survives, everything above it does not"
    );

    // --- and back up again from the floor ------------------------------------
    let (ok, out) = target.run(&db, &["up"]);
    assert!(ok, "db up from the floor failed: {out}");
    assert_eq!(
        registry_rows(&pool).await,
        applied,
        "a full up from the registry floor must rebuild the whole schema"
    );

    target.cleanup(&db, pool).await;
}

#[tokio::test]
async fn dry_run_changes_nothing() {
    let Some(target) = Target::from_env() else {
        eprintln!("SKIP: DATABASE_URL is unset, cannot exercise db up/db down");
        return;
    };
    let db = scratch_name("dryrun");
    let pool = target.create(&db).await;

    let (ok, out) = target.run(&db, &["up"]);
    assert!(ok, "db up failed: {out}");
    let applied = registry_rows(&pool).await;

    let (ok, out) = target.run(
        &db,
        &["down", "--dry-run", "--number", "2", "--allow-destructive"],
    );
    assert!(ok, "dry-run down failed: {out}");
    assert!(
        out.contains("would revert"),
        "dry run should name what it would do, got {out}"
    );
    assert_eq!(
        registry_rows(&pool).await,
        applied,
        "a dry run must not touch the registry"
    );

    let (ok, out) = target.run(&db, &["up", "--dry-run"]);
    assert!(ok, "dry-run up failed: {out}");
    assert_eq!(registry_rows(&pool).await, applied);

    target.cleanup(&db, pool).await;
}

/// A destructive command pointed at a database that is not this machine has to
/// stop and ask, not half-drop a schema.
#[tokio::test]
async fn destructive_commands_refuse_a_remote_target_without_acknowledgement() {
    let Some(target) = Target::from_env() else {
        eprintln!("SKIP: DATABASE_URL is unset, cannot exercise db down");
        return;
    };
    if target.is_local() {
        eprintln!(
            "SKIP: DATABASE_URL points at this machine ({}), nothing to refuse",
            target.host
        );
        return;
    }
    let db = scratch_name("remote");
    let pool = target.create(&db).await;

    let (ok, out) = target.run(&db, &["up"]);
    assert!(ok, "db up failed: {out}");
    let applied = registry_rows(&pool).await;

    let (ok, out) = target.run(&db, &["down", "--number", "1"]);
    assert!(!ok, "a non-local target must be refused, but it ran: {out}");
    assert!(
        out.contains("--allow-destructive"),
        "the refusal should say how to proceed, got {out}"
    );
    assert_eq!(
        registry_rows(&pool).await,
        applied,
        "the refusal happened before anything was reverted"
    );

    let (ok, out) = target.run(&db, &["reset"]);
    assert!(!ok, "reset must be refused too, but it ran: {out}");
    assert_eq!(registry_rows(&pool).await, applied);

    target.cleanup(&db, pool).await;
}

#[test]
fn every_committed_migration_is_revertible_or_the_floor() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/migrations");
    let mut up = Vec::new();
    let mut down = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("migrations directory") {
        let path = entry.expect("readable entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with(".down.sql") {
            down.push(name.replacen(".down.sql", "", 1));
        } else if name.ends_with(".sql") {
            up.push(name.trim_end_matches(".sql").to_string());
        }
    }

    let orphan_reverts: Vec<&String> = down
        .iter()
        .filter(|stem| !up.iter().any(|u| u == *stem))
        .collect();
    assert!(
        orphan_reverts.is_empty(),
        "revert scripts with no matching migration: {orphan_reverts:?}"
    );
}
