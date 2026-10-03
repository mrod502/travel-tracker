use std::{env::current_dir, fs::File, io::Write, path::PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Local, Timelike};
use clap::Args;
use sqlx::PgPool;

use crate::runner::Runner;
use crate::up::MigrationError;

/// What a fresh migration file says before its author has written any of it.
///
/// The `up` block is open and empty, the revert block is commented out, so the
/// file is inert: `db up` rejects a migration with no statements rather than
/// recording an empty one as applied, and `db down` refuses to revert a
/// migration that declares no revert. A half-written migration fails loudly
/// instead of looking finished.
///
/// The revert lines are commented out as `-- --migrate:down.begin`, with a space
/// between `--` and `migrate:`. That space is what makes the example inert: a
/// directive is only recognised when `migrate:` follows the opening `--`
/// immediately, so `--migrate:` inside an ordinary comment stays prose.
fn skeleton(name: &str) -> String {
    format!(
        "-- Migration: {name}\n\
         --\n\
         -- Everything between --migrate:up.begin and --migrate:up.end runs in one\n\
         -- transaction, together with the registry row that records this migration:\n\
         -- either all of it commits or none of it does.\n\
         \n\
         --migrate:up.begin\n\
         \n\
         -- Write what moves the schema forward.\n\
         \n\
         --migrate:up.end\n\
         \n\
         -- The revert lives in this file, beside the change it undoes, so the two\n\
         -- cannot drift apart. Uncomment the two directive lines below and write the\n\
         -- statements that undo the block above; `db down` refuses to revert a\n\
         -- migration with no down block, and rejects a down block with nothing in it.\n\
         --\n\
         -- A statement PostgreSQL will not run inside a transaction block\n\
         -- (CREATE INDEX CONCURRENTLY, for one) takes a --migrate:skipTx line\n\
         -- directly above it. That statement then commits on its own and the\n\
         -- migration stops being atomic, so it is worth wanting rarely.\n\
         \n\
         -- --migrate:down.begin\n\
         -- DROP TABLE {name};\n\
         -- --migrate:down.end\n"
    )
}

#[derive(Args, Debug, Clone)]

pub(crate) struct NewMigrationArgs {
    #[arg()]
    name: String,
}
impl NewMigrationArgs {
    fn get_file_name(&self, now: &DateTime<Local>) -> String {
        let t_str = format!(
            "{:04}{:02}{:02}{:02}{:02}_{}.sql",
            now.year(),
            now.month(),
            now.day(),
            now.hour(),
            now.minute(),
            self.name
        );
        t_str
    }

    fn pwd() -> Result<PathBuf, MigrationError> {
        match current_dir() {
            Ok(pb) => Ok(pb),
            Err(e) => Err(MigrationError::new_from("failed to find current dir", e)),
        }
    }

    fn get_full_file_path(&self, name: String) -> Result<PathBuf, MigrationError> {
        let mut path = Self::pwd()?;
        path.push("src/migrations");
        path.push(name);
        Ok(path)
    }
}

#[async_trait]
impl Runner for NewMigrationArgs {
    type RunError = MigrationError;
    async fn run(&self, _pool: Option<&PgPool>) -> Result<String, Self::RunError> {
        let now = Local::now();
        let file_name = self.get_file_name(&now);
        let path = self.get_full_file_path(file_name)?;
        // The file is written with content in it, so re-running the command must
        // not stamp over whatever the author has since put there.
        if path.exists() {
            return Err(MigrationError::new(format!(
                "{} already exists; not overwriting a migration",
                path.display()
            )));
        }

        let mut file = File::create(path.clone())
            .map_err(|e| MigrationError::new_from("failed to create migration", e))?;
        file.write_all(skeleton(&self.name).as_bytes())
            .map_err(|e| MigrationError::new_from("failed to write migration", e))?;

        Ok(format!(
            "created {} — write the up block, and uncomment the down block: `db up` rejects a \
             migration with no statements and `db down` will not revert one with no revert",
            path.display()
        ))
    }
}

#[cfg(test)]
pub mod test {
    use db::{DirectiveRegistry, MigrationParseError, MigrationParser, StandardMigrationParser};

    use crate::new::{NewMigrationArgs, skeleton};

    #[test]
    pub fn test_get_file_name() {
        let na = &NewMigrationArgs {
            name: "create_a_table".to_string(),
        };
        let now = chrono::Local::now();
        println!(
            "{:?}",
            na.get_full_file_path(na.get_file_name(&now)).unwrap()
        )
    }

    /// Nothing in a fresh file may run: every line is a comment, including the
    /// two directive lines that open the (empty) up block.
    #[test]
    pub fn skeleton_is_comments_only() {
        for line in skeleton("create_widgets").lines() {
            assert!(
                line.trim().is_empty() || line.trim_start().starts_with("--"),
                "skeleton line is not a comment: {line:?}"
            );
        }
    }

    /// The skeleton names the migration it was generated for, so a file opened
    /// months later still says what it was for.
    #[test]
    pub fn skeleton_names_the_migration() {
        assert!(skeleton("create_widgets").contains("create_widgets"));
    }

    /// The whole point of the skeleton's shape: until the author writes the up
    /// block, `db up` refuses the file instead of recording an empty migration
    /// as applied.
    ///
    /// The specific error also proves the commented-out `--migrate:down` example
    /// stayed prose. Had it been read as a directive, this file would fail as an
    /// unterminated block rather than as an up block with nothing in it.
    #[test]
    pub fn an_unwritten_migration_is_not_applied() {
        let registry = DirectiveRegistry::with_builtins().expect("built-ins are distinct");
        let error = StandardMigrationParser::in_memory()
            .parse(&skeleton("create_widgets"), &registry)
            .expect_err("a migration with no statements must be refused");

        assert!(
            matches!(error, MigrationParseError::EmptyUp { .. }),
            "expected an empty-up refusal, got {error}"
        );
    }
}
