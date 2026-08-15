//! Custom database type implementations

use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgArgumentBuffer, PgTypeInfo, PgValueRef, Postgres};
use sqlx::{Decode, Encode, Type};

/// Wrapper for PostGIS geography POINT type
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostgisPoint(pub geo_types::Point<f64>);

impl Type<Postgres> for PostgisPoint {
    fn type_info() -> PgTypeInfo {
        // PostGIS geography type - we use text format for encoding/decoding
        PgTypeInfo::with_name("geography")
    }
}

impl<'q> Encode<'q, Postgres> for PostgisPoint {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        // Encode as WKT (Well-Known Text) format
        let wkt = format!("SRID=4326;POINT({} {})", self.0.x(), self.0.y());
        Encode::<Postgres>::encode(wkt, buf)
    }
}

impl<'r> Decode<'r, Postgres> for PostgisPoint {
    fn decode(value: PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // Decode from WKT format
        let text = value.as_str()?;
        
        // Parse SRID=4326;POINT(lon lat) format
        if let Some(wkt) = text.strip_prefix("SRID=4326;POINT(") {
            if let Some(coords) = wkt.strip_suffix(")") {
                let parts: Vec<&str> = coords.split_whitespace().collect();
                if parts.len() == 2 {
                    let lon = parts[0].parse::<f64>()?;
                    let lat = parts[1].parse::<f64>()?;
                    return Ok(PostgisPoint(geo_types::Point::new(lon, lat)));
                }
            }
        }
        
        Err(format!("Failed to parse PostGIS point: {}", text).into())
    }
}
