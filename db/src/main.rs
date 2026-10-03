mod down;
mod exec;
mod file_attrs;
mod new;
mod registry;
mod runner;
mod up;
use clap::{Parser, Subcommand};
use dotenv::dotenv;
use down::{DownArgs, ResetArgs, destruction_guard};
use log4rs::{
    Config, Handle,
    append::console::ConsoleAppender,
    config::{Appender, Root},
};
use new::NewMigrationArgs;
use sqlx::{Pool, Postgres, postgres::PgPoolOptions};
use std::{env, error::Error, fmt::Display};
use up::UpArgs;

use crate::runner::Runner;

const MIGRATION_TIME_FMT: &str = "%Y%m%d%H%M";

pub fn setup_log(level: &str) -> Handle {
    let stdout = ConsoleAppender::builder().build();
    let log_filter = level
        .parse::<log::LevelFilter>()
        .unwrap_or(log::LevelFilter::Trace);
    let config = Config::builder()
        .appender(Appender::builder().build("stdout", Box::new(stdout)))
        //.logger(Logger::builder().build("app::backend::db", LevelFilter::Info))
        //.logger(
        //    Logger::builder()
        //        .appender("requests")
        //        .additive(false)
        //        .build("app::requests", LevelFilter::Info),
        //)
        .build(Root::builder().appender("stdout").build(log_filter))
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

#[derive(Subcommand, Debug)]
enum Command {
    NewMigration(NewMigrationArgs),
    Up(UpArgs),
    Down(DownArgs),
    Reset(ResetArgs),
}

impl Command {
    fn requires_db(&self) -> bool {
        match self {
            Command::NewMigration(_) => false,
            Command::Up(_) => true,
            Command::Down(_) => true,
            Command::Reset(_) => true,
        }
    }

    /// What a command would destroy, so the target can be checked before
    /// anything connects: `(description, is a dry run, operator acknowledged)`.
    fn destruction(&self) -> Option<(&'static str, bool, bool)> {
        match self {
            Command::NewMigration(_) | Command::Up(_) => None,
            Command::Down(args) => {
                Some(("revert migrations", args.dry_run, args.allow_destructive))
            }
            Command::Reset(args) => Some((
                "drop and re-apply the schema",
                args.dry_run,
                args.allow_destructive,
            )),
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
    // `_message` is accepted and dropped: `AppError` carries only a cause today,
    // so the callers' context strings never reach the output. Kept as an
    // underscored parameter rather than removed so the call sites stay readable
    // until the message is given a field and a place in `Display`.
    pub fn new(_message: &str, cause: impl Error + 'static) -> Self {
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
    #[arg(long, default_value = "trace", global = true)]
    log_level: String,

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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenv().ok();
    let mut app = App::parse();
    setup_log(&app.log_level);

    // Checked before connecting: a destructive command pointed at somebody
    // else's database should fail on the spot, not halfway through a drop.
    if let Some((verb, dry_run, acknowledged)) = app.command.destruction() {
        destruction_guard(verb, &app.host, app.port, &app.db, dry_run, acknowledged)
            .map_err(|e| Box::new(e) as Box<dyn Error>)?;
    }

    let conn = if app.command.requires_db() {
        Some(app.conn().await?)
    } else {
        None
    };

    let summary = match &app.command {
        Command::NewMigration(new_args) => new_args
            .run(conn.as_ref())
            .await
            .map_err(|e| Box::new(e) as Box<dyn Error>)?,
        Command::Up(up_args) => up_args
            .run(conn.as_ref())
            .await
            .map_err(|e| Box::new(e) as Box<dyn Error>)?,
        Command::Down(down_args) => down_args
            .run(conn.as_ref())
            .await
            .map_err(|e| Box::new(e) as Box<dyn Error>)?,
        Command::Reset(reset_args) => reset_args
            .run(conn.as_ref())
            .await
            .map_err(|e| Box::new(e) as Box<dyn Error>)?,
    };
    if !summary.is_empty() {
        println!("{}", summary);
    }
    Ok(())
}
