# Migrations Module

This module contains all database migration scripts for the message vault system.

## Overview

Database migrations are SQL files that define schema changes to be applied in sequence. Each migration file is timestamped and applied in order to update the database schema from one version to the next.

## Migration Structure

Each migration file follows this pattern:
- Timestamped filename (e.g., `202603081008_create_users.sql`)
- SQL statements that define the schema changes
- Migrations are applied sequentially in timestamp order

## Key Migrations

* `202603081008_create_users.sql` - Creates the initial users table
* `202603122043_create_messages.sql` - Creates the messages table
* `202603122124_create_message_events.sql` - Creates message events table
* `202603191724_add_auth_tokens.sql` - Adds authentication token columns to users
* `202603221149_add_user_tokens.sql` - Adds user tokens table
* `202603271732_add_user_roles.sql` - Adds user roles functionality
* `202603302051_create_invitations.sql` - Creates invitations table
* `202603302125_invitations_unique_idx.sql` - Adds unique index to invitations

## Usage

Migrations are applied automatically by running `cargo run --bin db`. The system will detect and apply any unapplied migrations in timestamp order.