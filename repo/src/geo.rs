//! The one place Rust derives an H3 cell from a coordinate.
//!
//! The database derives `occurrences.geo_cell_fine` and `geo_cell_macro` itself
//! with h3-pg:
//!
//! ```sql
//! geo_cell_fine  H3INDEX GENERATED ALWAYS AS (h3_latlng_to_cell(ST_Force2D(location::geometry), 9)) STORED
//! geo_cell_macro H3INDEX GENERATED ALWAYS AS
//!     (h3_cell_to_parent(h3_latlng_to_cell(ST_Force2D(location::geometry), 9), 6)) STORED
//! ```
//!
//! That leaves the server as the only party able to say which cell an occurrence
//! falls in, which is untenable once a node signs its location (a verifier must
//! be able to recompute what was signed) or relays a row (the relay's primary key
//! contains the macro cell). This module is the independent client-side
//! implementation those paths check themselves against — h3-pg's answer and this
//! one agreeing is the thing being asserted, so an h3-pg build that takes its
//! arguments in the v3 order, or a core version that disagrees, shows up as a
//! mismatch rather than quietly storing a wrong cell.
//!
//! # Resolutions
//!
//! Res 9 (`RESOLUTION_FINE`) and res 6 (`RESOLUTION_MACRO`) are fixed by the
//! schema and its indexes; nothing here chooses them independently.
//!
//! The macro cell is always taken as the *parent of the fine cell*, mirroring the
//! SQL above. Deriving res 6 straight from the coordinate is a different
//! computation and can land in another cell for a point close to a resolution
//! boundary.
//!
//! # Coordinate order
//!
//! Three orderings coexist in this codebase — the signed payload stores
//! `[lat, lon]`, `geo_types::Point` and PostGIS WKT are `(x = lon, y = lat)`, and
//! `nodes.fixed_lat, nodes.fixed_lon` are lat first. Every function here therefore
//! takes named `lat`/`lon` parameters rather than a tuple, and returns `(lat, lon)`.
//!
//! # Cell values
//!
//! A cell is an [`h3o::CellIndex`] above this module — the same type a
//! `H3INDEX` column holds, validated on construction, with h3's own
//! `parent`/`resolution`/`boundary` maths on it. No `i64` or `u64` cell crosses
//! this API in either direction: an integer cell cannot be shown to be a cell at
//! all, which is the mistake [`crate::types::H3Index`] exists to make
//! unrepresentable on the wire and this module exists to avoid in the code.
//!
//! # Spelling a cell
//!
//! [`CellIndex`]'s `Display` is lowercase hex with no leading zeros and no `0x`,
//! which is byte-for-byte what h3-pg's `h3index_out` prints, so a cell can be
//! pasted between `psql` and this crate unchanged. [`parse_cell`] additionally
//! accepts the decimal spelling and an `0x` prefix, because operators type both.
//!
//! # Example
//!
//! ```
//! use repo::geo;
//!
//! let fine = geo::fine_cell(40.6892, -74.0445)?; // Statue of Liberty
//! assert_eq!(geo::macro_cell(40.6892, -74.0445)?, geo::parent_cell(fine, geo::RESOLUTION_MACRO)?);
//! assert_eq!(geo::parse_cell(&fine.to_string())?, fine);
//! # Ok::<(), repo::geo::GeoError>(())
//! ```

use h3o::{CellIndex, LatLng, Resolution};
use thiserror::Error;

/// Resolution of `occurrences.geo_cell_fine`.
pub const RESOLUTION_FINE: Resolution = Resolution::Nine;

/// Resolution of `occurrences.geo_cell_macro` and of the cells in
/// `nodes.owns_geo_cells`.
pub const RESOLUTION_MACRO: Resolution = Resolution::Six;

/// Errors from the conversions in this module.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum GeoError {
    /// A coordinate is outside the range a location can legitimately have.
    ///
    /// h3 itself clamps a latitude past a pole and wraps a longitude past the
    /// antimeridian instead of failing, which would turn a broken GPS reading into
    /// a confidently wrong cell. Rejected here instead.
    #[error("invalid coordinate ({lat}, {lon}): latitude must be within -90..=90, longitude within -180..=180")]
    InvalidCoordinate { lat: f64, lon: f64 },

    /// The value is not a valid H3 cell index.
    #[error("not a valid H3 cell index: {0}")]
    InvalidCell(String),

    /// A cell cannot have a parent at a *finer* resolution than its own.
    #[error("cell {cell} is at resolution {cell_resolution}, so it has no parent at resolution {resolution}")]
    NoParent {
        cell: CellIndex,
        cell_resolution: Resolution,
        resolution: Resolution,
    },
}

/// Res 9 cell containing the point, as the value `occurrences.geo_cell_fine` stores.
pub fn fine_cell(lat: f64, lon: f64) -> Result<CellIndex, GeoError> {
    cell_from_latlng(lat, lon, RESOLUTION_FINE)
}

/// Res 6 cell containing the point, as `occurrences.geo_cell_macro` stores it.
///
/// Computed as the res 6 parent of the res 9 cell so that this mirrors
/// `h3_cell_to_parent(h3_latlng_to_cell(…, 9), 6)` exactly.
pub fn macro_cell(lat: f64, lon: f64) -> Result<CellIndex, GeoError> {
    parent_cell(fine_cell(lat, lon)?, RESOLUTION_MACRO)
}

/// Cell at `resolution` containing the point.
pub fn cell_from_latlng(lat: f64, lon: f64, resolution: Resolution) -> Result<CellIndex, GeoError> {
    if !is_valid_latlng(lat, lon) {
        return Err(GeoError::InvalidCoordinate { lat, lon });
    }

    // Only unreachable for a non-finite component, which the range check above
    // already caught — h3o itself clamps and wraps rather than rejecting.
    let point = LatLng::new(lat, lon).map_err(|_| GeoError::InvalidCoordinate { lat, lon })?;

    Ok(point.to_cell(resolution))
}

/// Parent of `cell` at `resolution`.
///
/// The mirror of `h3_cell_to_parent`. `resolution` must be coarser than (or equal
/// to) the resolution `cell` is already at.
pub fn parent_cell(cell: CellIndex, resolution: Resolution) -> Result<CellIndex, GeoError> {
    let cell_resolution = cell.resolution();

    cell.parent(resolution).ok_or(GeoError::NoParent {
        cell,
        cell_resolution,
        resolution,
    })
}

/// Centre of `cell` as `(lat, lon)`.
pub fn cell_to_latlng(cell: CellIndex) -> (f64, f64) {
    let point = LatLng::from(cell);

    (point.lat(), point.lng())
}

/// Reads a cell written by a human: `8b1d5d69154a1a9`, `0x8b1d5d69154a1a9`, or the
/// decimal form of the same value.
///
/// A `0x` prefix forces hex. Without one, a string of digits alone is decimal and
/// anything containing `a`-`f` is hex — the two forms overlap for a digit-only hex
/// string, so callers that know the expected resolution should check it with
/// [`CellIndex::resolution`], which turns a misread into an error rather than a
/// silent wrong cell.
///
/// h3o's own `FromStr` for a cell is the strict hex-only form; this is the lenient
/// one for operator input, and it validates the result either way.
pub fn parse_cell(raw: &str) -> Result<CellIndex, GeoError> {
    let raw = raw.trim();
    let digits = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);
    let forced_hex = digits.len() != raw.len();
    let decimal = !forced_hex
        && !digits
            .chars()
            .any(|c| c.is_ascii_hexdigit() && !c.is_ascii_digit());

    let invalid = || GeoError::InvalidCell(raw.to_owned());

    let bits = if decimal {
        digits.parse::<u64>().map_err(|_| invalid())?
    } else {
        u64::from_str_radix(digits, 16).map_err(|_| invalid())?
    };

    CellIndex::try_from(bits).map_err(|_| invalid())
}

fn is_valid_latlng(lat: f64, lon: f64) -> bool {
    lat.is_finite()
        && lon.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(description, lat, lon, fine cell, macro cell)`.
    ///
    /// The expected cells were produced once by h3o and pinned by hand: they are
    /// not self-verifying against h3o on their own, but they do turn a later h3o
    /// upgrade — or a disagreement with h3-pg on the host — into a diff here.
    /// Note the leading `89`/`86`: an h3 cell encodes its resolution in the second
    /// hex digit, so every fine cell below is res 9 and every macro cell res 6.
    ///
    /// Pinned as decimal `u64` and turned into cells by [`cell`], not through
    /// [`parse_cell`] — a parser that misread a cell must not be able to make the
    /// fixture it is being tested against look correct too.
    const FIXTURES: &[(&str, f64, f64, u64, u64)] = &[
        (
            "statue of liberty",
            40.6892,
            -74.0445,
            617733151067471871,
            604222352263217151,
        ),
        (
            "southern hemisphere",
            -33.8568,
            151.2153,
            620336640800063487,
            606825841984274431,
        ),
        (
            "null island",
            0.0,
            0.0,
            619056821840379903,
            605546023066009599,
        ),
        (
            "north pole",
            90.0,
            0.0,
            617048546304851967,
            603537747495354367,
        ),
        (
            "south pole",
            -90.0,
            0.0,
            621260594331254783,
            607749795481649151,
        ),
        (
            "antimeridian from the east",
            0.0,
            179.9999,
            619222233253675007,
            605711434470391807,
        ),
        (
            "antimeridian from the west",
            0.0,
            -179.9999,
            619222233253675007,
            605711434470391807,
        ),
        (
            "international date line, south",
            -17.7134,
            178.065,
            619725197346340863,
            606214398494638079,
        ),
        (
            "greenwich meridian",
            51.4779,
            0.0,
            617438095265497087,
            603927296465698815,
        ),
        (
            "two degrees east of the antimeridian",
            0.0,
            178.0,
            619220848338272255,
            605710049477656575,
        ),
        (
            "two degrees west of the antimeridian",
            0.0,
            -178.0,
            618973696320077823,
            605462897463656447,
        ),
    ];

    /// Reads a pinned fixture as the cell it names.
    fn cell(raw: u64) -> CellIndex {
        CellIndex::try_from(raw).expect("a fixture must be a valid h3 cell index")
    }

    #[test]
    fn fixtures_produce_the_pinned_cells() {
        for (name, lat, lon, fine, macro_) in FIXTURES {
            assert_eq!(fine_cell(*lat, *lon), Ok(cell(*fine)), "{name}: fine cell");
            assert_eq!(
                macro_cell(*lat, *lon),
                Ok(cell(*macro_)),
                "{name}: macro cell"
            );
        }
    }

    #[test]
    fn the_macro_cell_is_the_parent_of_the_fine_cell() {
        for (name, _, _, fine, macro_) in FIXTURES {
            assert_eq!(
                parent_cell(cell(*fine), RESOLUTION_MACRO),
                Ok(cell(*macro_)),
                "{name}: macro must be the res 6 parent of fine"
            );
        }
    }

    #[test]
    fn points_on_either_side_of_the_antimeridian_are_distinct_cells() {
        // The two fixtures right on the line share a cell because they are only
        // 22 m apart. Two degrees apart has to be a different answer on each side:
        // equal cells here would mean a longitude wrap had collapsed them.
        let east = fine_cell(0.0, 178.0).unwrap();
        let west = fine_cell(0.0, -178.0).unwrap();

        assert_ne!(east, west);
        assert_ne!(
            macro_cell(0.0, 178.0).unwrap(),
            macro_cell(0.0, -178.0).unwrap()
        );
    }

    #[test]
    fn the_fixtures_are_at_the_expected_resolutions() {
        for (name, _, _, fine, macro_) in FIXTURES {
            assert_eq!(
                cell(*fine).resolution(),
                RESOLUTION_FINE,
                "{name}: fine resolution"
            );
            assert_eq!(
                cell(*macro_).resolution(),
                RESOLUTION_MACRO,
                "{name}: macro resolution"
            );
        }
    }

    #[test]
    fn a_cells_centre_falls_back_inside_the_cell() {
        for (name, _, _, fine, macro_) in FIXTURES {
            for fixture in [*fine, *macro_] {
                let cell = cell(fixture);
                let (lat, lon) = cell_to_latlng(cell);
                assert_eq!(
                    cell_from_latlng(lat, lon, cell.resolution()),
                    Ok(cell),
                    "{name}: centre of {cell} should map back to it"
                );
            }
        }
    }

    #[test]
    fn nearby_points_move_between_fine_cells_but_not_macro_cells() {
        // Res 9 edges are ~170 m here and res 6 edges ~10 km, so walking a few
        // hundred metres has to cross fine cells while staying in one macro cell.
        let mut fine_cells = Vec::new();
        for step in 0..20 {
            let lat = 40.6892 + step as f64 * 0.001;
            let lon = -74.0445 + step as f64 * 0.001;
            fine_cells.push((fine_cell(lat, lon).unwrap(), macro_cell(lat, lon).unwrap()));
        }

        let macro_cells: Vec<CellIndex> = {
            let mut seen: Vec<CellIndex> = Vec::new();
            for (_, macro_) in &fine_cells {
                if !seen.contains(macro_) {
                    seen.push(*macro_);
                }
            }
            seen
        };
        let distinct_fine: Vec<CellIndex> = {
            let mut seen: Vec<CellIndex> = Vec::new();
            for (fine, _) in &fine_cells {
                if !seen.contains(fine) {
                    seen.push(*fine);
                }
            }
            seen
        };

        assert!(
            distinct_fine.len() > 1,
            "the walk should cross fine cell boundaries"
        );
        assert_eq!(
            macro_cells.len(),
            1,
            "the walk should stay inside one macro cell"
        );
    }

    #[test]
    fn a_coordinate_has_to_be_inside_the_world() {
        for (lat, lon) in [
            (91.0, 0.0),
            (-91.0, 0.0),
            (0.0, 180.0001),
            (0.0, -180.0001),
            (f64::NAN, 0.0),
            (0.0, f64::NAN),
            (f64::INFINITY, 0.0),
            (0.0, f64::NEG_INFINITY),
        ] {
            // Matched on the variant rather than compared: the error echoes the
            // input back, and NaN never equals itself.
            let error = fine_cell(lat, lon)
                .expect_err("({lat}, {lon}) should be rejected rather than clamped");
            assert!(
                matches!(error, GeoError::InvalidCoordinate { .. }),
                "({lat}, {lon}) gave {error}"
            );
        }
    }

    #[test]
    fn the_edges_of_the_world_are_inside_it() {
        assert!(fine_cell(90.0, 180.0).is_ok());
        assert!(fine_cell(-90.0, -180.0).is_ok());
    }

    #[test]
    fn the_schema_resolutions_are_pinned() {
        // The generated columns and their indexes are written for exactly these
        // two; drifting here would desynchronise every cell this crate derives
        // from what h3-pg stores.
        assert_eq!(u8::from(RESOLUTION_FINE), 9);
        assert_eq!(u8::from(RESOLUTION_MACRO), 6);

        // A resolution outside h3's range is not a value this API can even be
        // called with — h3o's `Resolution` is the guard, which is why there is no
        // InvalidResolution error to raise.
        assert!(Resolution::try_from(16u8).is_err());
        assert!(Resolution::try_from(0u8).is_ok());
        assert!(Resolution::try_from(15u8).is_ok());
    }

    #[test]
    fn a_cell_cannot_have_a_parent_at_a_finer_resolution() {
        let (_, _, _, _, macro_) = FIXTURES[0];
        assert_eq!(
            parent_cell(cell(macro_), RESOLUTION_FINE),
            Err(GeoError::NoParent {
                cell: cell(macro_),
                cell_resolution: RESOLUTION_MACRO,
                resolution: RESOLUTION_FINE
            })
        );
    }

    #[test]
    fn a_cell_can_be_its_own_parent() {
        let (_, _, _, fine, _) = FIXTURES[0];
        assert_eq!(parent_cell(cell(fine), RESOLUTION_FINE), Ok(cell(fine)));
    }

    #[test]
    fn hex_and_decimal_both_name_the_same_cell() {
        for (name, _, _, fine, macro_) in FIXTURES {
            for fixture in [*fine, *macro_] {
                let cell = cell(fixture);
                // Display is the h3-pg spelling: lowercase hex, no leading zeros,
                // no `0x`. Pinned here because `parse_cell` and the text wire
                // format both take it as given.
                let hex = cell.to_string();
                assert_eq!(hex, format!("{cell:x}"), "{name}: display is lower hex");
                assert!(!hex.starts_with("0x"), "{name}: bare hex {hex}");
                assert!(
                    hex.len() <= 16 && !hex.starts_with('0'),
                    "{name}: no leading zeros ({hex})"
                );

                assert_eq!(parse_cell(&hex), Ok(cell), "{name}: hex {hex}");
                assert_eq!(
                    parse_cell(&format!("{}", u64::from(cell))),
                    Ok(cell),
                    "{name}: decimal"
                );
                assert_eq!(
                    parse_cell(&format!("0x{hex}")),
                    Ok(cell),
                    "{name}: 0x-prefixed {hex}"
                );
                assert_eq!(
                    parse_cell(&format!("  {hex}  ")),
                    Ok(cell),
                    "{name}: padded {hex}"
                );
                assert_eq!(
                    parse_cell(&hex.to_uppercase()),
                    Ok(cell),
                    "{name}: upper hex {hex}"
                );
            }
        }
    }

    #[test]
    fn junk_is_not_a_cell() {
        for raw in [
            "",
            "   ",
            "not a cell",
            "0x",
            "-1",
            "0x10000000000000000",
            "8b1d5d69154a1a9z",
            "12",
        ] {
            assert!(
                parse_cell(raw).is_err(),
                "{raw:?} should not parse as a cell"
            );
        }
    }

    #[test]
    fn a_negative_value_is_not_a_cell() {
        // h3 reserves the top bit, so every real cell is non-negative.
        assert_eq!(
            parse_cell("-1"),
            Err(GeoError::InvalidCell("-1".to_owned()))
        );
    }
}
