//! Error-bounded display approximation; authored samples remain unchanged.
use crate::{Error, MAX_VIEW_POINTS, Result};

const EVALUATIONS_PER_POINT: usize = 32;
const MAX_COORDINATE: f64 = 65536.0;
type Point = [f64; 2];

/// Keep original vertices whose connecting chords approximate the projected
/// polyline within `tolerance`. An exhausted work budget retains original detail.
pub(super) fn points(points: Vec<Point>, tolerance: f64) -> Result<Vec<Point>> {
    let budget = points.len().saturating_mul(EVALUATIONS_PER_POINT);
    let (indices, _) = retained_indices(&points, tolerance, budget)?;
    if indices.len() == points.len() {
        return Ok(points);
    }
    indices.into_iter().map(|index| points.get(index).copied().ok_or(Error::Invalid("display approximation index"))).collect()
}

// Returns the distance-evaluation count so tests can verify the work bound.
// Pending intervals have disjoint interiors; copying them on exhaustion is O(N).
fn retained_indices(points: &[Point], tolerance: f64, budget: usize) -> Result<(Vec<usize>, usize)> {
    if points.len() > MAX_VIEW_POINTS {
        return Err(Error::Limit("display approximation input"));
    }
    if !tolerance.is_finite() || tolerance < 0.0 || points.iter().flatten().any(|coordinate| !coordinate.is_finite() || coordinate.abs() > MAX_COORDINATE) {
        return Err(Error::Invalid("display approximation input"));
    }
    if points.len() <= 2 || tolerance == 0.0 {
        return Ok(((0..points.len()).collect(), 0));
    }
    let mut keep = vec![false; points.len()];
    if let Some(first) = keep.first_mut() {
        *first = true;
    }
    if let Some(last) = keep.last_mut() {
        *last = true;
    }
    let mut intervals = vec![(0usize, points.len().saturating_sub(1))];
    let mut remaining = budget;
    while let Some((start, end)) = intervals.pop() {
        let intermediate = end.saturating_sub(start).saturating_sub(1);
        if intermediate == 0 {
            continue;
        }
        // Never accept a chord using only a partially scanned interval.
        if intermediate > remaining {
            keep.get_mut(start..=end).ok_or(Error::Invalid("display approximation interval"))?.fill(true);
            continue;
        }
        remaining = remaining.saturating_sub(intermediate);
        let a = points.get(start).copied().ok_or(Error::Invalid("display approximation start"))?;
        let b = points.get(end).copied().ok_or(Error::Invalid("display approximation end"))?;
        let delta = [b[0] - a[0], b[1] - a[1]];
        let length = delta[0].hypot(delta[1]);
        let direction = if length == 0.0 { [0.0, 0.0] } else { [delta[0] / length, delta[1] / length] };
        let from = start.saturating_add(1);
        let interval = points.get(from..end).ok_or(Error::Invalid("display approximation interval"))?;
        let mut largest = tolerance;
        let mut split = None;
        for (offset, point) in interval.iter().enumerate() {
            let distance = segment_distance(*point, a, direction, length);
            // Strict comparison chooses the first maximum on equal distances.
            if distance > largest {
                largest = distance;
                split = Some(from.saturating_add(offset));
            }
        }
        if let Some(index) = split {
            *keep.get_mut(index).ok_or(Error::Invalid("display approximation split"))? = true;
            // LIFO: process the left interval first for deterministic budgeting.
            intervals.push((index, end));
            intervals.push((start, index));
        }
    }
    let indices = keep.into_iter().enumerate().filter_map(|(index, retain)| retain.then_some(index)).collect();
    Ok((indices, budget.saturating_sub(remaining)))
}

fn segment_distance(point: Point, start: Point, direction: Point, length: f64) -> f64 {
    let relative = [point[0] - start[0], point[1] - start[1]];
    if length == 0.0 {
        return relative[0].hypot(relative[1]);
    }
    let along = (relative[0] * direction[0] + relative[1] * direction[1]).clamp(0.0, length);
    (relative[0] - along * direction[0]).hypot(relative[1] - along * direction[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    // Independent oracle: endpoint dot tests plus cross-product line distance,
    // rather than the production normalized/clamped closest-point calculation.
    fn oracle_distance(point: Point, a: Point, b: Point) -> f64 {
        let ab = [b[0] - a[0], b[1] - a[1]];
        let ap = [point[0] - a[0], point[1] - a[1]];
        let squared_length = ab[0] * ab[0] + ab[1] * ab[1];
        let dot = ap[0] * ab[0] + ap[1] * ab[1];
        if squared_length == 0.0 || dot <= 0.0 {
            return ap[0].hypot(ap[1]);
        }
        if dot >= squared_length {
            return (point[0] - b[0]).hypot(point[1] - b[1]);
        }
        (ab[0] * ap[1] - ab[1] * ap[0]).abs() / squared_length.sqrt()
    }

    fn assert_intervals(source: &[Point], retained: &[usize], tolerance: f64) {
        if source.is_empty() {
            assert!(retained.is_empty());
            return;
        }
        assert_eq!(retained.first(), Some(&0));
        assert_eq!(retained.last(), Some(&source.len().saturating_sub(1)));
        for pair in retained.windows(2) {
            let [start, end] = pair else { continue };
            assert!(start < end);
            for point in &source[*start..=*end] {
                let distance = oracle_distance(*point, source[*start], source[*end]);
                assert!(distance <= tolerance + 1e-10, "interval {start}..={end}: {distance} > {tolerance}");
            }
        }
    }

    fn curve(count: u32) -> Vec<Point> {
        (0..count)
            .map(|index| {
                let t = f64::from(index) / f64::from(count.saturating_sub(1));
                [20.0 + 1000.0 * t, 400.0 + 180.0 * (TAU * t).sin() + 30.0 * (5.0 * TAU * t).sin()]
            })
            .collect()
    }

    #[test]
    fn empty_short_and_zero_tolerance_paths_are_unchanged() {
        for source in [vec![], vec![[1.0, 2.0]], vec![[1.0, 2.0], [3.0, 4.0]]] {
            assert_eq!(points(source.clone(), 0.125).expect("bounded fixture"), source);
        }
        let source = curve(32);
        assert_eq!(points(source.clone(), 0.0).expect("zero tolerance"), source);
    }

    #[test]
    fn collinear_repeated_points_reduce_to_original_endpoints() {
        let source = vec![[0.0, 0.0], [0.0, 0.0], [1.0, 0.0], [1.0, 0.0], [2.0, 0.0]];
        let (indices, evaluations) = retained_indices(&source, 0.125, 32 * source.len()).expect("bounded fixture");
        assert_eq!(indices, vec![0, 4]);
        assert_eq!(evaluations, 3);
        assert_intervals(&source, &indices, 0.125);
        assert_eq!(points(source, 0.125).expect("bounded fixture"), vec![[0.0, 0.0], [2.0, 0.0]]);
    }

    #[test]
    fn finite_segment_distance_preserves_overshoot_and_reversal() {
        let source = vec![[0.0, 0.0], [10.0, 0.0], [5.0, 0.0]];
        let (indices, _) = retained_indices(&source, 0.125, 96).expect("bounded fixture");
        assert_eq!(indices, vec![0, 1, 2]);
        assert_intervals(&source, &indices, 0.125);
    }

    #[test]
    fn coincident_endpoints_preserve_visible_loops() {
        let source = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]];
        let (indices, _) = retained_indices(&source, 0.125, 160).expect("bounded loop");
        assert_eq!(indices, vec![0, 1, 2, 3, 4]);
        assert_intervals(&source, &indices, 0.125);
        let duplicate = vec![[2.0, 3.0]; 16];
        assert_eq!(points(duplicate, 0.125).expect("duplicate points"), vec![[2.0, 3.0]; 2]);
    }

    #[test]
    fn dense_curve_is_bounded_at_each_display_density_and_preserves_source() {
        let source = curve(8192);
        let original = source.clone();
        for density in [0.125, 1.0, 2.0, 16.0] {
            let tolerance = 0.125 / density;
            let (indices, evaluations) = retained_indices(&source, tolerance, source.len() * 32).expect("bounded curve");
            assert!(indices.len() < source.len() / 4);
            assert!(evaluations <= source.len() * 32);
            assert_intervals(&source, &indices, tolerance);
            let expected: Vec<_> = indices.iter().map(|index| source[*index]).collect();
            assert_eq!(points(source.clone(), tolerance).expect("bounded curve"), expected);
            assert_eq!(source, original);
        }
    }

    #[test]
    fn zero_or_insufficient_work_budget_keeps_the_entire_interval() {
        let source = curve(128);
        let all: Vec<_> = (0..source.len()).collect();
        for budget in [0, 1, 125] {
            let (indices, evaluations) = retained_indices(&source, 0.125, budget).expect("bounded fixture");
            assert_eq!(indices, all);
            assert_eq!(evaluations, 0);
            assert_intervals(&source, &indices, 0.125);
        }
    }

    #[test]
    fn work_exhaustion_retains_unfinished_intervals_without_discarding_pending_work() {
        let source = vec![[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [3.0, 0.0], [4.0, 0.0], [5.0, 0.0], [6.0, 10.0], [7.0, 5.0], [8.0, 0.0]];
        let (indices, evaluations) = retained_indices(&source, 0.125, 9).expect("bounded fixture");
        assert_eq!(indices, vec![0, 1, 2, 3, 4, 5, 6, 8]);
        assert_eq!(evaluations, 8);
        assert_intervals(&source, &indices, 0.125);
    }

    #[test]
    fn ties_self_crossings_and_hairpins_are_deterministic_and_bounded() {
        let source = vec![[0.0, 0.0], [1.0, 1.0], [2.0, -1.0], [3.0, 0.0], [0.0, 0.01], [2.0, 2.0], [0.0, 2.0], [2.0, 0.0]];
        for budget in [0, 6, 12, source.len() * 32] {
            let first = retained_indices(&source, 0.125, budget).expect("bounded fixture");
            let second = retained_indices(&source, 0.125, budget).expect("bounded fixture");
            assert_eq!(first, second);
            assert!(first.1 <= budget);
            assert_intervals(&source, &first.0, 0.125);
        }
    }

    #[test]
    fn malformed_and_oversized_inputs_fail_safely() {
        for tolerance in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(points(vec![[0.0, 0.0]], tolerance).is_err());
        }
        for coordinate in [65537.0, -65537.0, f64::NAN, f64::INFINITY] {
            assert!(points(vec![[coordinate, 0.0]], 0.125).is_err());
        }
        assert!(points(vec![[0.0, 0.0]; MAX_VIEW_POINTS + 1], 0.125).is_err());
    }
}
