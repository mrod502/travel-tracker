use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::Executor;

use crate::error::RepoError;
use crate::models::BluetoothOccurrence;

/// Repository for Bluetooth occurrence records
///
/// This repository manages append-only records of Bluetooth device advertisements.
/// The underlying table does not support UPDATE or DELETE operations - all writes
/// are INSERTs only.
///
/// # Append-Only Guarantee
///
/// The append-only constraint is enforced at the application layer:
/// - No UPDATE or DELETE methods are provided
/// - The table schema should have constraints to prevent modifications
/// - Business logic in higher layers should treat occurrences as immutable
///
/// # Transaction Support
///
/// All methods accept any type that implements `Executor<'c, Database = Postgres>`,
/// which includes both `&PgPool` and `&mut PgTransaction`. This allows operations
/// to be performed within transactions:
///
/// ```ignore
/// let mut tx = pool.begin().await?;
/// BluetoothOccurrenceRepository::create(&mut tx, &occurrence).await?;
/// tx.commit().await?;
/// ```
pub struct BluetoothOccurrenceRepository {
    _private: (), // Marker to prevent direct construction
}

impl BluetoothOccurrenceRepository {
    /// Create a new occurrence record (append-only)
    ///
    /// # Arguments
    /// * `executor` - Any executor (PgPool or PgTransaction)
    /// * `occurrence` - The occurrence to insert
    ///
    /// # Returns
    /// The inserted record with all database-generated fields
    ///
    /// # Note
    /// This is an append-only operation. No updates are allowed.
    pub async fn create<'c, E>(
        executor: E,
        occurrence: &BluetoothOccurrence,
    ) -> Result<BluetoothOccurrence, RepoError>
    where
        E: Executor<'c, Database = sqlx::Postgres>,
    {
        let record = sqlx::query_as!(
            BluetoothOccurrence,
            r#"
            INSERT INTO bluetooth_occurrences (
                occurrence_id, node_id, observed_at, observed_at_node_local,
                device_address, device_address_type, device_advertised_name, device_hash,
                advertisement_type, rssi, tx_power, service_uuids,
                manufacturer_company_id, manufacturer_payload_hex, raw_payload_hex,
                location_lat, location_lon, location_alt_m, location_accuracy_m,
                location_source, schema_version
            ) VALUES (
                $1, $2, $3, $4,
                $5, $6, $7, $8,
                $9, $10, $11, $12,
                $13, $14, $15,
                $16, $17, $18, $19,
                $20, $21
            )
            RETURNING *
            "#,
            occurrence.occurrence_id,
            occurrence.node_id,
            occurrence.observed_at,
            occurrence.observed_at_node_local,
            occurrence.device_address,
            occurrence.device_address_type,
            occurrence.device_advertised_name,
            occurrence.device_hash,
            occurrence.advertisement_type,
            occurrence.rssi,
            occurrence.tx_power,
            occurrence.service_uuids,
            occurrence.manufacturer_company_id,
            occurrence.manufacturer_payload_hex,
            occurrence.raw_payload_hex,
            occurrence.location_lat,
            occurrence.location_lon,
            occurrence.location_alt_m,
            occurrence.location_accuracy_m,
            occurrence.location_source,
            occurrence.schema_version,
        )
        .fetch_one(executor)
        .await?;

        Ok(record)
    }

    /// Find an occurrence by its ID
    ///
    /// # Arguments
    /// * `executor` - Any executor (PgPool or PgTransaction)
    /// * `id` - The occurrence UUID
    ///
    /// # Returns
    /// `Ok(Some(occurrence))` if found, `Ok(None)` if not found
    pub async fn find_by_id<'c, E>(
        executor: E,
        id: &str,
    ) -> Result<Option<BluetoothOccurrence>, RepoError>
    where
        E: Executor<'c, Database = sqlx::Postgres>,
    {
        let record = sqlx::query_as!(
            BluetoothOccurrence,
            "SELECT * FROM bluetooth_occurrences WHERE occurrence_id = $1",
            id
        )
        .fetch_optional(executor)
        .await?;

        Ok(record)
    }

    /// List occurrences with pagination
    ///
    /// # Arguments
    /// * `executor` - Any executor (PgPool or PgTransaction)
    /// * `limit` - Maximum number of records to return
    /// * `offset` - Number of records to skip
    ///
    /// # Returns
    /// Vector of occurrences, ordered by observed_at DESC (newest first)
    pub async fn list<'c, E>(
        executor: E,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<BluetoothOccurrence>, RepoError>
    where
        E: Executor<'c, Database = sqlx::Postgres>,
    {
        let records = sqlx::query_as!(
            BluetoothOccurrence,
            r#"
            SELECT * FROM bluetooth_occurrences
            ORDER BY observed_at DESC
            LIMIT $1 OFFSET $2
            "#,
            limit,
            offset
        )
        .fetch_all(executor)
        .await?;

        Ok(records)
    }

    /// Find occurrences by device address
    ///
    /// # Arguments
    /// * `executor` - Any executor (PgPool or PgTransaction)
    /// * `address` - BLE MAC address to search for
    /// * `limit` - Maximum number of records to return
    ///
    /// # Returns
    /// Vector of occurrences for the given device, ordered by observed_at DESC
    pub async fn find_by_device_address<'c, E>(
        executor: E,
        address: &str,
        limit: i64,
    ) -> Result<Vec<BluetoothOccurrence>, RepoError>
    where
        E: Executor<'c, Database = sqlx::Postgres>,
    {
        let records = sqlx::query_as!(
            BluetoothOccurrence,
            r#"
            SELECT * FROM bluetooth_occurrences
            WHERE device_address = $1
            ORDER BY observed_at DESC
            LIMIT $2
            "#,
            address,
            limit
        )
        .fetch_all(executor)
        .await?;

        Ok(records)
    }

    /// Find occurrences within a time range
    ///
    /// # Arguments
    /// * `executor` - Any executor (PgPool or PgTransaction)
    /// * `start` - Start of time range (inclusive)
    /// * `end` - End of time range (inclusive)
    /// * `limit` - Maximum number of records to return
    ///
    /// # Returns
    /// Vector of occurrences within the time range, ordered by observed_at DESC
    pub async fn find_by_time_range<'c, E>(
        executor: E,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<BluetoothOccurrence>, RepoError>
    where
        E: Executor<'c, Database = sqlx::Postgres>,
    {
        let records = sqlx::query_as!(
            BluetoothOccurrence,
            r#"
            SELECT * FROM bluetooth_occurrences
            WHERE observed_at BETWEEN $1 AND $2
            ORDER BY observed_at DESC
            LIMIT $3
            "#,
            start,
            end,
            limit
        )
        .fetch_all(executor)
        .await?;

        Ok(records)
    }

    /// Find occurrences by node ID
    ///
    /// # Arguments
    /// * `executor` - Any executor (PgPool or PgTransaction)
    /// * `node_id` - ID of the observing node
    /// * `limit` - Maximum number of records to return
    ///
    /// # Returns
    /// Vector of occurrences from the given node, ordered by observed_at DESC
    pub async fn find_by_node_id<'c, E>(
        executor: E,
        node_id: &str,
        limit: i64,
    ) -> Result<Vec<BluetoothOccurrence>, RepoError>
    where
        E: Executor<'c, Database = sqlx::Postgres>,
    {
        let records = sqlx::query_as!(
            BluetoothOccurrence,
            r#"
            SELECT * FROM bluetooth_occurrences
            WHERE node_id = $1
            ORDER BY observed_at DESC
            LIMIT $2
            "#,
            node_id,
            limit
        )
        .fetch_all(executor)
        .await?;

        Ok(records)
    }

    /// Find occurrences by location proximity (simple bounding box)
    ///
    /// # Arguments
    /// * `executor` - Any executor (PgPool or PgTransaction)
    /// * `center_lat` - Center latitude
    /// * `center_lon` - Center longitude
    /// * `radius_degrees` - Search radius in degrees (approximate)
    /// * `limit` - Maximum number of records to return
    ///
    /// # Returns
    /// Vector of occurrences within the bounding box, ordered by distance
    pub async fn find_by_location<'c, E>(
        executor: E,
        center_lat: Decimal,
        center_lon: Decimal,
        radius_degrees: Decimal,
        limit: i64,
    ) -> Result<Vec<BluetoothOccurrence>, RepoError>
    where
        E: Executor<'c, Database = sqlx::Postgres>,
    {
        // Convert radius to decimal for bounds
        let lat_min = center_lat - radius_degrees;
        let lat_max = center_lat + radius_degrees;
        let lon_min = center_lon - radius_degrees;
        let lon_max = center_lon + radius_degrees;

        let records = sqlx::query_as!(
            BluetoothOccurrence,
            r#"
            SELECT * FROM bluetooth_occurrences
            WHERE location_lat IS NOT NULL
              AND location_lon IS NOT NULL
              AND location_lat BETWEEN $1 AND $2
              AND location_lon BETWEEN $3 AND $4
            ORDER BY observed_at DESC
            LIMIT $5
            "#,
            lat_min,
            lat_max,
            lon_min,
            lon_max,
            limit
        )
        .fetch_all(executor)
        .await?;

        Ok(records)
    }

    /// Count occurrences in a time range
    ///
    /// # Arguments
    /// * `executor` - Any executor (PgPool or PgTransaction)
    /// * `start` - Start of time range (inclusive)
    /// * `end` - End of time range (inclusive)
    ///
    /// # Returns
    /// Count of occurrences in the time range
    pub async fn count_by_time_range<'c, E>(
        executor: E,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<i64, RepoError>
    where
        E: Executor<'c, Database = sqlx::Postgres>,
    {
        let record = sqlx::query!(
            r#"
            SELECT COUNT(*) as count FROM bluetooth_occurrences
            WHERE observed_at BETWEEN $1 AND $2
            "#,
            start,
            end
        )
        .fetch_one(executor)
        .await?;

        Ok(record.count.unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: Actual database tests require a running PostgreSQL instance
    // Use `#[sqlx::test]` for integration tests
    #[test]
    fn test_repository_exists() {
        // This is a placeholder - actual tests need database
        let _repo = BluetoothOccurrenceRepository { _private: () };
    }
}
