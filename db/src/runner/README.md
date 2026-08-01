# DB Runner Module

This module contains the core logic for running database migrations.

## Key Functions

* `run_migrations` - Main function that applies unapplied migrations
* Migration tracking and status checking
* Database connection management

## Implementation Details

The runner uses `sqlx` to connect to PostgreSQL and executes migration files in timestamp order. It tracks applied migrations in the database to prevent reapplication.