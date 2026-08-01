use std::{env::current_dir, fs::File, path::PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Local, Timelike};
use clap::Args;
use sqlx::PgPool;

use crate::{runner::Runner, up::MigrationError};

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
        let path = self.get_full_file_path(self.get_file_name(&now))?;
        match File::create(path.clone()) {
            Ok(_) => Ok(path.to_str().unwrap().into()),
            Err(e) => Err(Self::RunError::new(e)),
        }
    }
}

#[cfg(test)]
pub mod test {
    use chrono::Local;

    use crate::new::NewMigrationArgs;

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
}
