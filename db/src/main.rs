mod file_attrs;
mod new;
mod runner;
mod up;
use async_trait::async_trait;
use clap::{Args, Parser, Subcommand};
use dotenv::dotenv;
use log4rs::{
    Config, Handle,
    append::console::ConsoleAppender,
    config::{Appender, Root},
};
use new::NewMigrationArgs;
use sqlx::{Executor, PgPool, Pool, Postgres, postgres::PgPoolOptions};
use std::{env, error::Error, fmt::Display};
use up::UpArgs;

use crate::{runner::Runner, up::MigrationError};

const MIGRATION_INIT: &str = "CREATE TABLE IF NOT EXISTS migrations (
    id BIGSERIAL PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL,
    executed_at TIMESTAMPTZ NOT NULL default now()
)";
const MIGRATION_TIME_FMT: &str = "%Y%m%d%H%M";

pub fn setup_log() -> Handle {
    let stdout = ConsoleAppender::builder().build();
    let config = Config::builder()
        .appender(Appender::builder().build("stdout", Box::new(stdout)))
        //.logger(Logger::builder().build("app::backend::db", LevelFilter::Info))
        //.logger(
        //    Logger::builder()
        //        .appender("requests")
        //        .additive(false)
        //        .build("app::requests", LevelFilter::Info),
        //)
        .build(
            Root::builder()
                .appender("stdout")
                .build(log::LevelFilter::Trace),
        )
        .unwrap();
    log4rs::init_config(config).unwrap()
}

pub trait Dsn {
    fn dsn(&self) -> String;

    fn password() -> String {
        env::var("DB_PASSWORD").unwrap()
    }
    #[allow(async_fn_in_trait)]
    async fn conn(&mut self) -> Result<Pool<Postgres>, AppError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&self.dsn())
            .await
            .map_err(|e| AppError::new("Failed to connect to database", e))?;
        Ok(pool)
    }
}

#[async_trait]
impl Runner for DownArgs {
    type RunError = MigrationError;
    async fn run(&self, _conn: Option<&Pool<Postgres>>) -> Result<String, Self::RunError> {
        Ok("".into())
    }
}

#[derive(Args, Debug, Clone)]
struct DownArgs {}

#[derive(Subcommand, Debug)]
enum Command {
    NewMigration(NewMigrationArgs),
    Up(UpArgs),
    Down(DownArgs),
}

impl Command {
    fn requires_db(&self) -> bool {
        match self {
            Command::NewMigration(_) => false,
            Command::Up(_) => true,
            Command::Down(_) => true,
        }
    }
}
#[derive(Debug)]
pub struct AppError {
    cause: Option<Box<dyn Error>>,
}

impl Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AppError")
    }
}

impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.cause.as_ref().map(|e| e.as_ref())
    }

    fn description(&self) -> &str {
        "description() is deprecated; use Display"
    }

    fn cause(&self) -> Option<&dyn Error> {
        self.cause.as_ref().map(|e| e.as_ref())
    }
}

impl AppError {
    pub fn new(message: &str, cause: impl Error + 'static) -> Self {
        AppError {
            cause: Some(Box::new(cause)),
        }
    }
}

#[derive(Parser, Debug)]
struct App {
    #[arg(long, default_value = "localhost", global = true)]
    host: String,
    #[arg(long, default_value_t = 5432, global = true)]
    port: u16,
    #[arg(long, default_value = "postgres", global = true)]
    user: String,
    #[arg(long, default_value = "postgres", global = true)]
    db: String,

    #[command(subcommand)]
    command: Command,
}

impl App {
    fn dsn(&self) -> String {
        format!(
            "postgres://{}:{}@{}:{}/{}",
            self.user,
            Self::password(),
            self.host,
            self.port,
            self.db
        )
    }
    fn password() -> String {
        env::var("DB_PASSWORD").unwrap()
    }

    async fn conn(&mut self) -> Result<Pool<Postgres>, AppError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&self.dsn())
            .await
            .map_err(|e| AppError::new("Failed to connect to database", e))?;
        Ok(pool)
    }

    async fn migration_init(&self, conn: &Pool<Postgres>) -> Result<(), AppError> {
        conn.execute(MIGRATION_INIT)
            .await
            .map_err(|e| AppError::new("Failed to initialize migrations table", e))?;
        Ok(())
    }
}

#[async_trait]
impl Runner for App {
    type RunError = AppError;
    async fn run(&self, conn: Option<&Pool<Postgres>>) -> Result<String, Self::RunError> {
        log::info!("running");
        if let Some(pool) = conn {
            if self.command.requires_db() {
                log::info!("running migrations");
                self.migration_init(pool).await?;
            }
        }
        Ok("".into())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    setup_log();
    dotenv().ok();
    let mut app = App::parse();
    let mut c: PgPool = PgPool::connect_lazy(&app.dsn()).unwrap();
    let conn = if app.command.requires_db() {
        c = app.conn().await?;
        app.migration_init(&c).await?;
        Some(&c)
    } else {
        None
    };

    match &app.command {
        Command::NewMigration(new_args) => new_args.run(conn),
        Command::Up(up_args) => up_args.run(conn),
        Command::Down(down_args) => down_args.run(conn),
    }
    .await?;
    Ok(())
}
