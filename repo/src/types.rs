//! Custom database type implementations.
//!
//! This is the only place in the workspace that knows how a Postgres type that
//! sqlx has no built-in support for crosses the wire. Both types here were
//! pinned against a live server rather than inferred from documentation; the
//! measurements are in
//! [`.knowledge/implementation/ws0-h3index-wire-results.md`](../../.knowledge/implementation/ws0-h3index-wire-results.md).
//!
//! The rule this module exists to enforce: **never** `SELECT *` into a model
//! whose custom-typed columns are declared as some ordinary Rust integer or
//! string. sqlx matches decode impls on the type OID, so an `h3index` column in
//! an `Option<i64>` field is a decode error — and on an
//! `INSERT … RETURNING *` that error arrives *after* the row has been written.
//!
//! These types are wire-format plumbing only. The value an `h3index` column
//! holds is an [`h3o::CellIndex`], and that is the type the rest of the
//! workspace passes around; [`H3Index`] exists solely because Rust's orphan rules
//! stop anyone outside `sqlx` from implementing `Type`/`Encode`/`Decode` on a
//! foreign type, and converting is free in both directions.

use h3o::CellIndex;
use serde::{Deserialize, Serialize};
use sqlx::encode::IsNull;
use sqlx::postgres::{
    PgArgumentBuffer, PgHasArrayType, PgTypeInfo, PgValueFormat, PgValueRef, Postgres,
};
use sqlx::{Decode, Encode, Type};

type DynErr = Box<dyn std::error::Error + Send + Sync>;

/// h3-pg's `h3index`, carrying an [`h3o::CellIndex`].
///
/// The cell itself is h3o's — validated, with h3's own parent/resolution/geometry
/// maths on it. This newtype contributes exactly one thing sqlx needs: a local
/// type to hang `Type`, `Encode` and `Decode` off, so the parameter and result
/// are announced as `h3index` rather than as `int8` or `text`.
///
/// Measured behaviour this mirrors:
///
/// * `h3index` is a pass-by-value base type (`typlen` 8, category `U`), **not** a
///   domain over `bigint`.
/// * The casts `bigint → h3index` and `h3index → bigint` exist but are
///   **explicit**, so `WHERE geo_cell_macro = $1` with an `int8` parameter fails
///   with `operator does not exist: h3index = bigint`. Declaring the parameter as
///   `h3index` — which is what `Type` does here — is what makes the comparison
///   resolve and the index usable.
/// * On the wire it is the cell index as 8 **big-endian** bytes. The text form is
///   lowercase hex with the leading zeros stripped (`89f058792d3ffff`), which is
///   what [`h3o::CellIndex`]'s own `Display` prints, so both formats decode.
///
/// A value off the wire that is not a well-formed h3 cell is rejected rather than
/// carried, so an `h3index` column that somehow contains junk fails the read
/// instead of handing back a cell that resolves to no geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct H3Index(pub CellIndex);

impl H3Index {
    /// The h3 cell this crosses the wire as.
    pub fn cell(self) -> CellIndex {
        self.0
    }
}

impl From<CellIndex> for H3Index {
    fn from(cell: CellIndex) -> Self {
        Self(cell)
    }
}

impl From<H3Index> for CellIndex {
    fn from(cell: H3Index) -> Self {
        cell.0
    }
}

/// The spelling `h3index_out` uses, by way of h3o's `Display`.
impl std::fmt::Display for H3Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Type<Postgres> for H3Index {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_name("h3index")
    }
}

impl PgHasArrayType for H3Index {
    fn array_type_info() -> PgTypeInfo {
        PgTypeInfo::with_name("_h3index")
    }
}

impl Encode<'_, Postgres> for H3Index {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, DynErr> {
        buf.extend_from_slice(&u64::from(self.0).to_be_bytes());
        Ok(IsNull::No)
    }

    fn size_hint(&self) -> usize {
        8
    }
}

impl Decode<'_, Postgres> for H3Index {
    fn decode(value: PgValueRef<'_>) -> Result<Self, DynErr> {
        let bytes = value.as_bytes()?;

        let cell = match value.format() {
            PgValueFormat::Binary => {
                let raw: [u8; 8] = bytes
                    .try_into()
                    .map_err(|_| format!("h3index arrived as {} bytes, expected 8", bytes.len()))?;
                CellIndex::try_from(u64::from_be_bytes(raw))?
            }
            // Only reachable when a query opts into the text format. h3index_in
            // accepts hex and decimal alike, and so does parse_cell.
            PgValueFormat::Text => {
                let text = std::str::from_utf8(bytes)?;
                crate::geo::parse_cell(text).map_err(|e| e.to_string())?
            }
        };

        Ok(Self(cell))
    }
}

/// PostGIS `GEOGRAPHY(POINT, 4326)`.
///
/// sqlx asks the server for **binary**, and PostGIS answers with EWKB, so this
/// type speaks EWKB in both directions:
///
/// ```text
/// 01 | 01 00 00 20 | e6 10 00 00 | <lon f64 LE> | <lat f64 LE>
/// ^ little-endian   ^ type 1 +      ^ SRID 4326
///                   ^ SRID flag
/// ```
///
/// Writing WKT into a binary-format parameter — which is what an earlier version
/// of this type did — fails at the server with `Invalid endian flag value
/// encountered`, and reading binary geography as text fails with an invalid-UTF-8
/// error. Both directions fail loudly, so a location can never be stored or read
/// as something other than what it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostgisPoint(pub geo_types::Point<f64>);

/// SRID `location` is constrained to.
const WGS84_SRID: u32 = 4326;

/// EWKB flag: the SRID follows the geometry type.
const EWKB_SRID_FLAG: u32 = 0x2000_0000;
/// EWKB flag: Z coordinates are present.
const EWKB_Z_FLAG: u32 = 0x8000_0000;
/// EWKB flag: M coordinates are present.
const EWKB_M_FLAG: u32 = 0x4000_0000;
/// WKB geometry type for Point (1), plus the classic +1000/+2000/+3000 Z/M variants.
const WKB_POINT: u32 = 1;

impl Type<Postgres> for PostgisPoint {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_name("geography")
    }
}

impl Encode<'_, Postgres> for PostgisPoint {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, DynErr> {
        // 1 endianness + 4 type + 4 SRID + 2 coordinates.
        let mut bytes = [0u8; 25];
        bytes[0] = 1; // little-endian
        bytes[1..5].copy_from_slice(&(WKB_POINT | EWKB_SRID_FLAG).to_le_bytes());
        bytes[5..9].copy_from_slice(&WGS84_SRID.to_le_bytes());
        bytes[9..17].copy_from_slice(&self.0.x().to_le_bytes());
        bytes[17..25].copy_from_slice(&self.0.y().to_le_bytes());

        buf.extend_from_slice(&bytes);
        Ok(IsNull::No)
    }

    fn size_hint(&self) -> usize {
        25
    }
}

impl Decode<'_, Postgres> for PostgisPoint {
    fn decode(value: PgValueRef<'_>) -> Result<Self, DynErr> {
        let bytes = value.as_bytes()?;

        match value.format() {
            PgValueFormat::Binary => ewkb_point(bytes).map(PostgisPoint),
            // geography_out prints hex EWKB; a hand-written query may wrap one in
            // `ST_AsText`, which yields `SRID=4326;POINT(lon lat)`.
            PgValueFormat::Text => {
                let text = std::str::from_utf8(bytes)?;
                if let Some(point) = wkt_point(text) {
                    return Ok(PostgisPoint(point));
                }
                let hex = hex_bytes(text)
                    .ok_or_else(|| format!("unrecognised geography value: {text}"))?;
                ewkb_point(&hex).map(PostgisPoint)
            }
        }
    }
}

/// Reads the `(x, y)` out of an EWKB point, ignoring SRID, Z, and M.
fn ewkb_point(bytes: &[u8]) -> Result<geo_types::Point<f64>, DynErr> {
    let Some(endianness) = bytes.first().copied() else {
        return Err("empty geography value".into());
    };
    let little_endian = match endianness {
        1 => true,
        0 => false,
        other => return Err(format!("invalid EWKB endian flag {other}").into()),
    };

    let read_u32 = |chunk: &[u8]| -> u32 {
        let raw: [u8; 4] = chunk.try_into().unwrap_or([0; 4]);
        if little_endian {
            u32::from_le_bytes(raw)
        } else {
            u32::from_be_bytes(raw)
        }
    };
    let read_f64 = |chunk: &[u8]| -> f64 {
        let raw: [u8; 8] = chunk.try_into().unwrap_or([0; 8]);
        if little_endian {
            f64::from_le_bytes(raw)
        } else {
            f64::from_be_bytes(raw)
        }
    };

    if bytes.len() < 5 {
        return Err("truncated EWKB: no geometry type".into());
    }

    let type_word = read_u32(&bytes[1..5]);
    let flags = type_word & 0xE000_0000;
    let kind = type_word & 0x0FFF_FFFF;

    if kind % 1000 != WKB_POINT {
        return Err(format!("geography is an EWKB type {kind}, not a point").into());
    }

    let mut offset = 5;
    if flags & EWKB_SRID_FLAG != 0 {
        if bytes.len() < offset + 4 {
            return Err("truncated EWKB: SRID cut off".into());
        }
        offset += 4;
    }

    let has_z = flags & EWKB_Z_FLAG != 0 || (1001..2000).contains(&kind) || kind >= 3001;
    let has_m = flags & EWKB_M_FLAG != 0 || (2001..3000).contains(&kind) || kind >= 3001;

    let coordinate_len = 8 * (2 + usize::from(has_z) + usize::from(has_m));
    if bytes.len() < offset + coordinate_len {
        return Err("truncated EWKB: missing coordinates".into());
    }

    let x = read_f64(&bytes[offset..offset + 8]);
    let y = read_f64(&bytes[offset + 8..offset + 16]);

    Ok(geo_types::Point::new(x, y))
}

/// `SRID=4326;POINT(lon lat)` or bare `POINT(lon lat)`, case-insensitive.
fn wkt_point(text: &str) -> Option<geo_types::Point<f64>> {
    let upper = text.to_ascii_uppercase();
    let start = upper.find("POINT(")?;

    // Only an `SRID=…;` prefix may precede the geometry keyword; anything else
    // means this is not a point we were handed, and guessing would put a
    // location on the row that did not come from one.
    let prefix = text[..start].trim();
    if !(prefix.is_empty()
        || (prefix.to_ascii_uppercase().starts_with("SRID=") && prefix.ends_with(';')))
    {
        return None;
    }

    let coords = text[start + "POINT(".len()..].split(')').next()?;
    let mut values = coords.split_whitespace();
    let x = values.next()?.parse::<f64>().ok()?;
    let y = values.next()?.parse::<f64>().ok()?;

    Some(geo_types::Point::new(x, y))
}

fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact 25 bytes a live PostGIS 3.6.4 sent for the Statue of Liberty.
    const LIBERTY_EWKB: [u8; 25] = [
        0x01, 0x01, 0x00, 0x00, 0x20, 0xe6, 0x10, 0x00, 0x00, 0x02, 0x2b, 0x87, 0x16, 0xd9, 0x82,
        0x52, 0xc0, 0x9c, 0x33, 0xa2, 0xb4, 0x37, 0x58, 0x44, 0x40,
    ];

    #[test]
    fn reads_the_bytes_a_real_server_sent() {
        let point = ewkb_point(&LIBERTY_EWKB).unwrap();
        assert!((point.x() - -74.0445).abs() < 1e-9, "lon was {}", point.x());
        assert!((point.y() - 40.6892).abs() < 1e-9, "lat was {}", point.y());
    }

    #[test]
    fn encoding_then_reading_gives_the_same_point_back() {
        // The encoder's output has to be what the decoder (and PostGIS) expects,
        // byte for byte — including little-endian ordering and the SRID flag.
        let point = PostgisPoint(geo_types::Point::new(-74.0445, 40.6892));
        let mut buf = PgArgumentBuffer::default();
        assert!(matches!(point.encode_by_ref(&mut buf), Ok(IsNull::No)));

        let encoded: &[u8] = buf.as_ref();
        assert_eq!(encoded.len(), 25);
        assert_eq!(encoded[0], 1);
        assert_eq!(&encoded[5..9], &4326u32.to_le_bytes());

        let decoded = ewkb_point(encoded).unwrap();
        assert_eq!((decoded.x(), decoded.y()), (-74.0445, 40.6892));
    }

    #[test]
    fn big_endian_input_is_read_too() {
        let mut bytes = Vec::new();
        bytes.push(0); // big-endian
        bytes.extend_from_slice(&(WKB_POINT | EWKB_SRID_FLAG).to_be_bytes());
        bytes.extend_from_slice(&4326u32.to_be_bytes());
        bytes.extend_from_slice(&2.5f64.to_be_bytes());
        bytes.extend_from_slice(&(-1.25f64).to_be_bytes());

        let point = ewkb_point(&bytes).unwrap();
        assert_eq!((point.x(), point.y()), (2.5, -1.25));
    }

    #[test]
    fn z_coordinates_are_read_past_not_misread() {
        let mut bytes = Vec::new();
        bytes.push(1);
        bytes.extend_from_slice(&(WKB_POINT | EWKB_SRID_FLAG | EWKB_Z_FLAG).to_le_bytes());
        bytes.extend_from_slice(&4326u32.to_le_bytes());
        bytes.extend_from_slice(&1.0f64.to_le_bytes());
        bytes.extend_from_slice(&2.0f64.to_le_bytes());
        bytes.extend_from_slice(&99.0f64.to_le_bytes()); // altitude, ignored

        let point = ewkb_point(&bytes).unwrap();
        assert_eq!((point.x(), point.y()), (1.0, 2.0));
    }

    #[test]
    fn text_forms_both_decode() {
        let from_wkt = wkt_point("SRID=4326;POINT(-74.0445 40.6892)").unwrap();
        assert_eq!((from_wkt.x(), from_wkt.y()), (-74.0445, 40.6892));

        let bare = wkt_point("point(-74.0445 40.6892)").unwrap();
        assert_eq!((bare.x(), bare.y()), (-74.0445, 40.6892));

        let hex = hex_bytes(
            &LIBERTY_EWKB
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
        )
        .unwrap();
        let from_hex = ewkb_point(&hex).unwrap();
        assert_eq!((from_hex.x(), from_hex.y()), (-74.0445, 40.6892));
    }

    #[test]
    fn junk_is_rejected_rather_than_guessed() {
        for bad in [
            b"".as_slice(),
            b"\x02".as_slice(),
            b"\x01\x02\x00\x00\x20\xe6\x10\x00\x00".as_slice(),
            b"\x01\x02\x00\x00\x00\xe6\x10\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".as_slice(),
        ] {
            assert!(ewkb_point(bad).is_err(), "{bad:02x?} should not decode");
        }
    }

    #[test]
    fn h3_index_encodes_the_eight_big_endian_bytes_of_the_cell() {
        let cell = crate::geo::macro_cell(40.6892, -74.0445).unwrap();
        let wrapped = H3Index::from(cell);

        assert_eq!(wrapped.cell(), cell);
        assert_eq!(CellIndex::from(wrapped), cell);

        let mut buf = PgArgumentBuffer::default();
        assert!(matches!(wrapped.encode_by_ref(&mut buf), Ok(IsNull::No)));
        let encoded: &[u8] = buf.as_ref();
        assert_eq!(encoded, &u64::from(cell).to_be_bytes());
    }

    /// The bytes `h3index_send` puts on the wire, measured off a live h3-pg 4.2.3
    /// rather than derived from a spec: cell `89f058792d3ffff` travels as
    /// `08 9f 05 87 92 d3 ff ff`, i.e. the index big-endian and unsigned.
    #[test]
    fn the_encoding_is_what_the_server_itself_sends() {
        let measured_cell: CellIndex = "89f058792d3ffff".parse().unwrap();
        let mut buf = PgArgumentBuffer::default();
        assert!(matches!(
            H3Index::from(measured_cell).encode_by_ref(&mut buf),
            Ok(IsNull::No)
        ));

        assert_eq!(
            <[u8; 8]>::try_from(buf.as_ref()).unwrap(),
            [0x08, 0x9f, 0x05, 0x87, 0x92, 0xd3, 0xff, 0xff]
        );
    }

    #[test]
    fn h3_index_prints_the_way_h3index_out_does() {
        let cell = crate::geo::macro_cell(40.6892, -74.0445).unwrap();

        // Lowercase hex, no leading zeros, no `0x` — so a value copied out of
        // `psql` parses back, and one printed here can be pasted into SQL.
        assert_eq!(H3Index::from(cell).to_string(), cell.to_string());
    }
}
