# DB Main Module

This module contains the main CLI utility for applying database migrations.

## Key Functions

* `main` - Entry point that parses command line arguments and applies migrations
* Migration execution logic using the runner

## Usage

Run database migrations with: `cargo run --bin db`

The binary will automatically detect and apply any unapplied migrations in timestamp order.