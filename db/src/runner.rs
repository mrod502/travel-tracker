use std::error::Error;

use async_trait::async_trait;
use sqlx::PgPool;

#[async_trait]
pub(crate) trait Runner {
    type RunError: Error;
    async fn run(&self, conn: Option<&PgPool>) -> Result<String, Self::RunError>;
}
