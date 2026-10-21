//! Distances and the geohash search for "open shelters within r metres" (spec 4.5, 6.3.3).

use firestore::{FirestoreDb, path};
use geohash::Coord;

use crate::error::ApiError;
use crate::shelters::{COLLECTION, ShelterDoc, ShelterStatus};
use crate::store::fs_error;

const EARTH_RADIUS_M: f64 = 6_371_008.8;
const METERS_PER_DEGREE: f64 = EARTH_RADIUS_M * std::f64::consts::PI / 180.0;
/// Characters of the `shelters.geohash` field (spec 7).
pub const STORED_PRECISION: usize = 9;
const MAX_PRECISION: usize = STORED_PRECISION;

pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (phi1, phi2) = (lat1.to_radians(), lat2.to_radians());
    let d_phi = (lat2 - lat1).to_radians();
    let d_lambda = (lon2 - lon1).to_radians();
    let a = (d_phi / 2.0).sin().powi(2) + phi1.cos() * phi2.cos() * (d_lambda / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * a.sqrt().min(1.0).asin()
}

pub fn encode(lat: f64, lon: f64, precision: usize) -> String {
    geohash::encode(Coord { x: lon, y: lat }, precision).expect("coordinates are validated")
}

/// Length in metres of the shorter side of a cell of `precision` characters near `lat`.
/// A geohash alternates longitude and latitude bits, starting with longitude.
fn min_cell_side_m(precision: usize, lat: f64) -> f64 {
    let bits = 5 * precision;
    let (lon_bits, lat_bits) = (bits.div_ceil(2), bits / 2);
    let lat_degrees = 180.0 / 2f64.powi(lat_bits as i32);
    let lon_degrees = 360.0 / 2f64.powi(lon_bits as i32);
    // Meridians close in towards the poles; the cell is narrowest at its far edge.
    let far_edge = (lat.abs() + lat_degrees).min(89.9);
    (lat_degrees * METERS_PER_DEGREE)
        .min(lon_degrees * METERS_PER_DEGREE * far_edge.to_radians().cos())
}

/// The longest geohash whose cell is at least `radius_m` on every side, so that every point
/// within `radius_m` of the centre lies in the centre cell or one of its 8 neighbours.
pub fn precision_for(radius_m: f64, lat: f64) -> usize {
    (1..=MAX_PRECISION)
        .rev()
        .find(|p| min_cell_side_m(*p, lat) >= radius_m)
        .unwrap_or(1)
}

/// The cell of the point and its 8 neighbours (fewer at the poles, where neighbours repeat).
pub fn cells(lat: f64, lon: f64, precision: usize) -> Vec<String> {
    let center = encode(lat, lon, precision);
    let mut cells = vec![center.clone()];
    if let Ok(n) = geohash::neighbors(&center) {
        for cell in [n.n, n.ne, n.e, n.se, n.s, n.sw, n.w, n.nw] {
            if !cells.contains(&cell) {
                cells.push(cell);
            }
        }
    }
    cells
}

pub struct Near {
    pub shelter: ShelterDoc,
    pub distance_m: f64,
}

/// Open shelters within `radius_m` of the point, nearest first: 9 range queries on `geohash`,
/// then the exact distance. Reads through `db`, so inside a transaction they belong to it.
pub async fn open_shelters_within(
    db: &FirestoreDb,
    lat: f64,
    lon: f64,
    radius_m: f64,
) -> Result<Vec<Near>, ApiError> {
    let mut found = Vec::new();
    for cell in cells(lat, lon, precision_for(radius_m, lat)) {
        let upper = format!("{cell}~");
        let in_cell: Vec<ShelterDoc> = db
            .fluent()
            .select()
            .from(COLLECTION)
            .filter(|q| {
                q.for_all([
                    q.field(path!(ShelterDoc::status)).eq(ShelterStatus::Open),
                    q.field(path!(ShelterDoc::geohash))
                        .greater_than_or_equal(&cell),
                    q.field(path!(ShelterDoc::geohash)).less_than(&upper),
                ])
            })
            .obj()
            .query()
            .await
            .map_err(fs_error)?;
        for shelter in in_cell {
            let distance_m = haversine_m(lat, lon, shelter.lat, shelter.lon);
            if distance_m <= radius_m {
                found.push(Near {
                    shelter,
                    distance_m,
                });
            }
        }
    }
    found.sort_by(|a, b| a.distance_m.total_cmp(&b.distance_m));
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A point `meters` north of `(lat, lon)`.
    fn north(lat: f64, meters: f64) -> f64 {
        lat + meters / METERS_PER_DEGREE
    }

    /// A point `meters` east of `(lat, lon)`.
    fn east(lat: f64, lon: f64, meters: f64) -> f64 {
        lon + meters / (METERS_PER_DEGREE * lat.to_radians().cos())
    }

    #[test]
    fn haversine_matches_known_distances() {
        assert_eq!(haversine_m(-30.0, -51.0, -30.0, -51.0), 0.0);
        // One degree of latitude.
        let degree = haversine_m(0.0, 0.0, 1.0, 0.0);
        assert!((degree - 111_195.0).abs() < 50.0, "{degree}");
        // Porto Alegre to Sao Paulo, about 852 km.
        let pa_sp = haversine_m(-30.0346, -51.2177, -23.5505, -46.6333);
        assert!((pa_sp - 852_000.0).abs() < 5_000.0, "{pa_sp}");
        // Symmetric, and across the antimeridian the short way.
        assert!(
            (haversine_m(10.0, 20.0, 11.0, 22.0) - haversine_m(11.0, 22.0, 10.0, 20.0)).abs()
                < 1e-6
        );
        let across = haversine_m(0.0, 179.9995, 0.0, -179.9995);
        assert!((across - 111.2).abs() < 1.0, "{across}");
    }

    #[test]
    fn haversine_of_small_offsets_is_accurate_to_the_metre() {
        let lat = -30.0;
        for meters in [50.0, 80.0, 99.0, 101.0, 150.0] {
            let d = haversine_m(lat, -51.0, north(lat, meters), -51.0);
            assert!((d - meters).abs() < 0.5, "{meters}: {d}");
            let d = haversine_m(lat, -51.0, lat, east(lat, -51.0, meters));
            assert!((d - meters).abs() < 0.5, "{meters}: {d}");
        }
    }

    #[test]
    fn precision_is_the_longest_cell_that_still_covers_the_radius() {
        // The spec's examples: 7 for 100 m, 3 for 50 km.
        assert_eq!(precision_for(100.0, -30.0), 7);
        assert_eq!(precision_for(50_000.0, -30.0), 3);
        assert_eq!(precision_for(100.0, 0.0), 7);
        // Bigger radius, shorter hash; nothing smaller than the stored hash.
        assert_eq!(precision_for(4.0, 0.0), 9);
        assert_eq!(precision_for(500_000.0, 0.0), 2);
        assert_eq!(precision_for(1_000_000.0, 0.0), 1);
        assert_eq!(precision_for(10_000_000.0, 0.0), 1);
        for radius in [10.0, 100.0, 5_000.0, 50_000.0] {
            let p = precision_for(radius, -30.0);
            assert!(min_cell_side_m(p, -30.0) >= radius);
            if p < MAX_PRECISION {
                assert!(min_cell_side_m(p + 1, -30.0) < radius, "{radius}");
            }
        }
    }

    #[test]
    fn precision_shrinks_where_the_meridians_close_in() {
        // At 70 degrees a 7-character cell is under 100 m wide.
        assert!(precision_for(100.0, 70.0) < 7);
    }

    #[test]
    fn nine_cells_are_distinct_and_cover_the_point() {
        let cells = cells(-30.0346, -51.2177, 7);
        assert_eq!(cells.len(), 9);
        let mut unique = cells.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 9);
        assert!(cells.iter().all(|c| c.len() == 7));
        assert_eq!(cells[0], encode(-30.0346, -51.2177, 7));
    }

    /// Whether `other` is reachable through the 9 cells around `origin`.
    fn covered(origin: (f64, f64), other: (f64, f64), precision: usize) -> bool {
        let target = encode(other.0, other.1, precision);
        cells(origin.0, origin.1, precision).contains(&target)
    }

    #[test]
    fn a_neighbour_just_over_a_cell_border_is_still_found() {
        // Walk to the east edge of a 7-character cell and stand on both sides of it.
        let (lat, lon) = (-30.0, -51.0);
        let bbox = geohash::decode_bbox(&encode(lat, lon, 7)).unwrap();
        let border = bbox.max().x;
        let inside = (lat, border - 0.000_01);
        let outside = (lat, border + 0.000_01);
        assert_ne!(
            encode(inside.0, inside.1, 7),
            encode(outside.0, outside.1, 7)
        );
        assert!(haversine_m(inside.0, inside.1, outside.0, outside.1) < 3.0);
        assert!(covered(inside, outside, 7) && covered(outside, inside, 7));
    }

    #[test]
    fn every_point_within_the_radius_falls_in_the_nine_cells() {
        // Many origins, 100 m in eight directions: a miss here would let two shelters at
        // 99 m both be created.
        let radius = 100.0;
        for (lat, lon) in [
            (-30.0346, -51.2177),
            (-3.1, -60.0),
            (0.0, 0.0),
            (-33.7, -53.4),
            (60.0, 10.0),
        ] {
            let p = precision_for(radius, lat);
            for k in 0..360 {
                let angle = f64::from(k).to_radians();
                let near = (
                    north(lat, 99.0 * angle.sin()),
                    east(lat, lon, 99.0 * angle.cos()),
                );
                assert!(
                    covered((lat, lon), near, p),
                    "({lat}, {lon}) at {k} degrees"
                );
            }
        }
    }

    #[test]
    fn cells_wrap_across_the_antimeridian_and_the_prime_meridian() {
        let p = precision_for(100.0, 0.0);
        // 60 m apart, either side of 180 degrees.
        let west_side = (0.0, 179.999_75);
        let east_side = (0.0, -179.999_75);
        assert!(haversine_m(west_side.0, west_side.1, east_side.0, east_side.1) < 60.0);
        assert!(covered(west_side, east_side, p) && covered(east_side, west_side, p));
        // And either side of 0 degrees.
        let a = (-1.0, -0.000_3);
        let b = (-1.0, 0.000_3);
        assert!(covered(a, b, p) && covered(b, a, p));
    }

    #[test]
    fn fifty_kilometres_are_covered_by_precision_three() {
        let (lat, lon) = (-30.0, -51.0);
        let p = precision_for(50_000.0, lat);
        for k in 0..360 {
            let angle = f64::from(k).to_radians();
            let near = (
                north(lat, 49_900.0 * angle.sin()),
                east(lat, lon, 49_900.0 * angle.cos()),
            );
            assert!(covered((lat, lon), near, p), "{k} degrees");
        }
    }
}
