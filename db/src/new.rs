use std::{env::current_dir, fs::File, io::Write, path::PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Local, Timelike};
use clap::Args;
use sqlx::PgPool;

use crate::up::MigrationError;
use crate::{runner::Runner, up::DOWN_SUFFIX};

/// What a fresh revert script says before its author has written any of it.
///
/// It is deliberately not empty-but-usable: `db down` refuses to revert a
/// migration whose script contains no statements, so a half-written migration
/// fails loudly instead of quietly dropping its registry row.
fn down_script_stub(up_file: &str) -> String {
    format!(
        "-- Revert {up_file}\n\
         --\n\
         -- Write the statements that undo the migration here. `db down` runs this\n\
         -- file and removes the migration's registry row in one transaction, so\n\
         -- either both happen or neither does.\n\
         --\n\
         -- Until there is at least one statement in here, `db down` refuses to\n\
         -- revert {up_file}. That is the point: an empty revert script would\n\
         -- forget the migration and leave the schema claiming to be what it was.\n\
         --\n\
         -- Prefer RESTRICT over CASCADE, and check the order: the migrations after\n\
         -- this one have already been reverted, so anything they created is gone.\n"
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

    /// The revert script paired with a migration, sharing its timestamp.
    fn get_down_file_name(&self, now: &DateTime<Local>) -> String {
        let stem = self
            .get_file_name(now)
            .strip_suffix(".sql")
            .map(str::to_string)
            .expect("get_file_name always ends in .sql");
        format!("{stem}{DOWN_SUFFIX}")
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
        let up_name = self.get_file_name(&now);
        let path = self.get_full_file_path(up_name.clone())?;
        let mut up_file = File::create(path.clone())
            .map_err(|e| MigrationError::new_from("failed to create migration", e))?;
        // A migration with nothing in it is a no-op the registry will still
        // record, so leave the file empty for the author to fill in.
        up_file
            .flush()
            .map_err(|e| MigrationError::new_from("failed to write migration", e))?;

        let down_path = self.get_full_file_path(self.get_down_file_name(&now))?;
        if down_path.exists() {
            return Err(MigrationError::new(format!(
                "{} already exists; not overwriting a revert script",
                down_path.display()
            )));
        }
        let mut down_file = File::create(&down_path)
            .map_err(|e| MigrationError::new_from("failed to create revert script", e))?;
        down_file
            .write_all(down_script_stub(&up_name).as_bytes())
            .map_err(|e| MigrationError::new_from("failed to write revert script", e))?;

        Ok(format!(
            "created {} and {} — the revert script has to be written before \
             `db down` will undo this migration",
            path.display(),
            down_path.display()
        ))
    }
}

#[cfg(test)]
pub mod test {
    use chrono::Local;

    use crate::new::{NewMigrationArgs, down_script_stub};
    use crate::up::DOWN_SUFFIX;

    #[test]
    pub fn test_get_file_name() {
        let na = &NewMigrationArgs {
            name: "create_a_table".to_string(),
        };
        let now = Local::now();
        println!(
            "{:?}",
            na.get_full_file_path(na.get_file_name(&now)).unwrap()
        )
    }

    /// The pair has to share one timestamp, or `db down` cannot find the
    /// script that belongs to a migration.
    #[test]
    pub fn revert_script_is_paired_with_its_migration() {
        let na = &NewMigrationArgs {
            name: "create_a_table".to_string(),
        };
        let now = Local::now();

        let up = na.get_file_name(&now);
        let down = na.get_down_file_name(&now);

        assert_eq!(
            down,
            format!("{}{DOWN_SUFFIX}", up.strip_suffix(".sql").unwrap()),
            "the pair must share one timestamp"
        );
        assert_eq!(&up[..12], &down[..12]);
    }

    /// The stub explains the contract; it must not accidentally contain a
    /// runnable statement, which would make an unwritten revert look complete.
    #[test]
    pub fn stub_is_comments_only() {
        let stub = down_script_stub("202601010000_example.sql");
        for line in stub.lines() {
            assert!(
                line.trim().is_empty() || line.trim_start().starts_with("--"),
                "stub line is not a comment: {line:?}"
            );
        }
        assert!(stub.contains("202601010000_example.sql"));
    }
}
