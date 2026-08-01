use crate::{MIGRATION_TIME_FMT, file_attrs::FileAttrs, runner::Runner};
use async_trait::async_trait;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use clap::Args;
use sqlx::{AssertSqlSafe, Executor, PgPool, Pool, Postgres, Row, Transaction};
use std::{error::Error, fmt::Display, fs::File, io::Read, path::PathBuf, usize};
use walkdir::WalkDir;

#[derive(Debug, Default)]
pub struct MigrationError {
    src: Option<Box<dyn Error>>,
    #[allow(dead_code)]
    reason: String,
}

impl Error for MigrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.src.as_deref()
    }

    fn cause(&self) -> Option<&dyn Error> {
        self.source()
    }
}

impl MigrationError {
    pub fn new(reason: impl ToString) -> MigrationError {
        MigrationError {
            src: None,
            reason: reason.to_string(),
        }
    }

    pub fn new_from<E: Error + 'static>(message: &str, source: E) -> Self {
        Self {
            reason: String::from(message),
            src: Some(Box::new(source)),
        }
    }
}

impl Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Args, Debug, Clone)]
pub struct UpArgs {
    #[arg(long, short, default_value_t = 0)]
    pub number: usize,
    #[arg(long, default_value = "localhost")]
    pub host: String,
    #[arg(long, default_value_t = 5432)]
    pub port: u16,
    #[arg(long, default_value = "postgres")]
    pub user: String,
    #[arg(long, default_value = "postgres")]
    pub db: String,
    #[arg(long, default_value = "./src/migrations")]
    pub migrations_path: String,
}

#[async_trait]
impl Runner for UpArgs {
    type RunError = MigrationError;
    async fn run(&self, maybe_conn: Option<&PgPool>) -> Result<String, MigrationError> {
        let conn = match maybe_conn {
            Some(c) => c,
            None => return Err(MigrationError::new("no conn provided")),
        };
        log::info!("running:{:?}", self);
        let latest_migration = self.get_latest_applied_migration(conn).await?;
        log::trace!("latest migration:{}", latest_migration.to_rfc3339());
        let mut migrations_to_run: Vec<FileAttrs> = WalkDir::new(&self.migrations_path)
            .into_iter()
            .filter_map(|v| -> Option<FileAttrs> {
                let de = match v {
                    Ok(de) => de,
                    Err(_) => return None,
                };
                let pth = de.clone().into_path();
                log::info!("path:{}", pth.to_str().unwrap());
                let Some(ext) = pth.extension() else {
                    return None;
                };
                log::trace!("ext:{}", ext.to_str().unwrap());
                let Some(ext_str) = ext.to_str() else {
                    return None;
                };
                if ext_str != "sql" {
                    return None;
                }

                let Ok(file_attrs) = Self::parse_file_name(&pth) else {
                    return None;
                };
                if file_attrs.created_at <= latest_migration {
                    return None;
                }
                log::info!("file_attrs:{:?}", file_attrs);

                Some(file_attrs)
            })
            .collect();
        migrations_to_run.sort();
        for mig in migrations_to_run {
            self.apply_migration(mig, conn).await?;
        }

        Ok("".into())
    }
}

impl UpArgs {
    fn parse_file_name<'b>(pth: &'b PathBuf) -> Result<FileAttrs, MigrationError> {
        let Some(os_name) = pth.file_name() else {
            return Err(MigrationError::new("no filename"));
        };
        let Some(full_name) = os_name.to_str() else {
            return Err(MigrationError::new("failed conversion to str"));
        };
        let Some((date_str, rest)) = full_name.split_once("_") else {
            return Err(MigrationError::new(full_name));
        };

        let created_at = match NaiveDateTime::parse_from_str(date_str, MIGRATION_TIME_FMT) {
            Ok(f) => f,
            Err(e) => {
                return Err(MigrationError::new(e));
            }
        }
        .and_local_timezone(Local)
        .unwrap();

        let Some((name, sql)) = rest.split_once(".") else {
            return Err(MigrationError::new("no extension"));
        };
        if sql != "sql" {
            return Err(MigrationError::new(format!(
                "invalid file extension: {}",
                sql
            )));
        }
        let attrs: FileAttrs = FileAttrs {
            name: name.into(),
            created_at,
            full_path: pth.clone(),
        };
        Ok(attrs)
    }
    async fn apply_migration(
        &self,
        attrs: FileAttrs,
        conn: &Pool<Postgres>,
    ) -> Result<(), MigrationError> {
        log::info!("attrs:{:?}", attrs);
        let mut tx = match conn.begin().await {
            Ok(t) => t,
            Err(e) => return Err(MigrationError::new_from("failed to begin tx", e)),
        };
        let migration = match self.read_file(&attrs.full_path) {
            Ok(v) => v,
            Err(e) => return Err(MigrationError::new(e)),
        };
        let _ = self.add_migration_to_registry(&mut tx, &attrs).await?;

        let statements = Self::split_query(&migration);

        for statement in statements {
            let _ = match sqlx::query(AssertSqlSafe(statement))
                .execute(&mut *tx)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(MigrationError::new_from("failed to execute migration", e));
                }
            };
        }

        match tx.commit().await {
            Ok(_) => Ok(()),
            Err(e) => Err(MigrationError::new(format!(
                "failed to commit migration:{}",
                e
            ))),
        }
    }
    async fn add_migration_to_registry(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        attrs: &FileAttrs,
    ) -> Result<(), MigrationError> {
        let q = sqlx::query("INSERT INTO migrations (name, created_at) VALUES ($1,$2)")
            .bind(attrs.name.clone())
            .bind(attrs.created_at);

        match tx.execute(q).await {
            Ok(_) => Ok(()),
            Err(e) => Err(MigrationError::new_from("failed to register migration", e)),
        }
    }

    fn split_query(q: &str) -> Vec<String> {
        q.split(";").map(|v| v.to_string()).collect()
    }

    fn read_file<'a, 'b>(&self, p: &'a PathBuf) -> Result<String, MigrationError> {
        let mut f = match File::open(p) {
            Ok(v) => v,
            Err(e) => return Err(MigrationError::new_from("failed to open file", e)),
        };
        let mut out = String::new();
        let _ = match f.read_to_string(&mut out) {
            Ok(v) => v,
            Err(e) => return Err(MigrationError::new_from("failed to read file", e)),
        };
        Ok(out)
    }

    async fn get_latest_applied_migration(
        &self,
        conn: &Pool<Postgres>,
    ) -> Result<DateTime<Local>, MigrationError> {
        let v: Result<Option<DateTime<Local>>, sqlx::Error> = match conn
            .fetch_one("SELECT MAX(created_at) FROM migrations")
            .await
        {
            Ok(v) => v.try_get(0),
            Err(e) => return Err(MigrationError::new(e)),
        };

        let maybe_dt = match v {
            Ok(maybe_dt) => maybe_dt,
            Err(e) => return Err(MigrationError::new(e)),
        };
        match maybe_dt {
            Some(dt) => Ok(dt.into()),
            None => Ok(Local.with_ymd_and_hms(0, 1, 1, 0, 0, 0).unwrap()),
        }
    }
}

#[cfg(test)]
mod test {
    use std::path::PathBuf;

    use crate::up::UpArgs;

    #[test]
    fn test_parse_file_name() {
        let mut pb = PathBuf::new();
        pb.push("db/src/migrations/202603081    008_create_users.sql");

        log::info!("{:?}", pb);
        let res = UpArgs::parse_file_name(&pb);
        log::info!("{:?}", res.unwrap());
    }
}
