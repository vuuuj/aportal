//! Smart snapping logic
//!
//! Two modes:
//! - XY alignment snap: while moving/selecting, align to a nearby region edge
//! - Edge-gap snap: two element edges can touch with a configurable gap
//!   Hysteresis: after snapping, move away 2x the distance to release, avoiding edge jitter

use super::common::rgb;
use super::state::{EditorState, Element, HDC};

/// Single-axis snap: nearest candidate only + hysteresis release
fn snap_axis(
    new_pos: i32,
    candidates: &[(i32, i32)], // (target_value, distance)
    prev_snapped: Option<i32>,
    snap_dist: i32,
) -> (i32, Option<i32>) {
    // Hysteresis: once snapped, hold until moved away snap_dist*2
    if let Some(prev) = prev_snapped {
        if (new_pos - prev).abs() < snap_dist * 2 {
            return (prev, Some(prev));
        }
    }
    // Not snapped or released: find the nearest candidate
    if let Some(&(target, dist)) = candidates.iter().min_by_key(|(_, d)| *d) {
        if dist <= snap_dist {
            return (target, Some(target));
        }
    }
    (new_pos, None)
}

/// Selecting-phase snap: check whether the mouse is near an existing region's edge
pub fn snap_selection(st: &mut EditorState, x: i32, y: i32) -> (i32, i32) {
    let snap_dist = st.snap_distance;

    let mut x_candidates: Vec<(i32, i32)> = Vec::new();
    let mut y_candidates: Vec<(i32, i32)> = Vec::new();

    // Only capture elements' source rects are candidates (UI elements have none):
    // the old version treated UI zero placeholder rects as candidates too, pulling snap to the origin
    for e in &st.elements {
        let Some(src) = &e.source else { continue };
        let sx = src.x as i32;
        let sy = src.y as i32;
        let sw = src.width as i32;
        let sh = src.height as i32;
        x_candidates.push((sx, (x - sx).abs()));
        x_candidates.push((sx + sw, (x - (sx + sw)).abs()));
        y_candidates.push((sy, (y - sy).abs()));
        y_candidates.push((sy + sh, (y - (sy + sh)).abs()));
    }

    let (snap_x, sx_opt) = snap_axis(x, &x_candidates, st.snapped_x, snap_dist);
    let (snap_y, sy_opt) = snap_axis(y, &y_candidates, st.snapped_y, snap_dist);

    st.snapped_x = sx_opt;
    st.snapped_y = sy_opt;

    (snap_x, snap_y)
}

/// Arranging-phase snap: check whether the moving element nears other elements' edges
/// With tian snap: edge/gap snapping disabled; only the moving element's "center" aligns to other elements' 3x3 intersections (9 points).
/// Without tian snap: edge alignment + edge-gap snap (with gap).
pub fn snap_arrangement(st: &mut EditorState, idx: usize, new_x: i32, new_y: i32) -> (i32, i32) {
    if idx >= st.elements.len() {
        return (new_x, new_y);
    }

    let snap_dist = st.snap_distance;
    let (moving_w, moving_h) = (st.elements[idx].display.width, st.elements[idx].display.height);
    let mut x_candidates: Vec<(i32, i32)> = Vec::new();
    let mut y_candidates: Vec<(i32, i32)> = Vec::new();

    if st.snap_tian {
        // Tian snap: only the "moving element center" aligns to other elements' 3x3 intersections (9 points).
        // x/y must hit the same node simultaneously; no per-axis line snapping (that would act like XY snap and pull wrongly).
        let cx = new_x + moving_w / 2;
        let cy = new_y + moving_h / 2;
        let mut best: Option<(i32, i32)> = None;
        let mut best_cost = i32::MAX;
        for (i, o) in st.elements.iter().enumerate() {
            if i == idx {
                continue;
            }
            let other = &o.display;
            let (ox, oy, ow, oh) = (other.x, other.y, other.width, other.height);
            for gx in [ox, ox + ow / 2, ox + ow] {
                for gy in [oy, oy + oh / 2, oy + oh] {
                    let dx = gx - cx;
                    let dy = gy - cy;
                    if dx.abs() <= snap_dist && dy.abs() <= snap_dist {
                        let cost = dx.abs() + dy.abs();
                        if cost < best_cost {
                            best_cost = cost;
                            best = Some((gx, gy));
                        }
                    }
                }
            }
        }
        st.snapped_x = None;
        st.snapped_y = None;
        return match best {
            Some((tx, ty)) => ((tx - moving_w / 2).max(0), (ty - moving_h / 2).max(0)),
            None => (new_x, new_y),
        };
    } else {
        let snap_gap = st.snap_gap;
        let moving_r = new_x + moving_w;
        let moving_b = new_y + moving_h;

        for (i, o) in st.elements.iter().enumerate() {
            if i == idx {
                continue;
            }
            let other = &o.display;
            let ox = other.x;
            let oy = other.y;
            let or = other.x + other.width;
            let ob = other.y + other.height;

            // X candidates: left/right alignment + edge-gap (with gap)
            x_candidates.push((ox, (new_x - ox).abs()));
            x_candidates.push((or, (new_x - or).abs()));
            x_candidates.push((ox - moving_w, (moving_r - ox).abs()));
            x_candidates.push((or - moving_w, (moving_r - or).abs()));
            x_candidates.push((or + snap_gap, (new_x - (or + snap_gap)).abs()));
            x_candidates.push((ox - moving_w - snap_gap, (moving_r - (ox - snap_gap)).abs()));

            // Y candidates
            y_candidates.push((oy, (new_y - oy).abs()));
            y_candidates.push((ob, (new_y - ob).abs()));
            y_candidates.push((oy - moving_h, (moving_b - oy).abs()));
            y_candidates.push((ob - moving_h, (moving_b - ob).abs()));
            y_candidates.push((ob + snap_gap, (new_y - (ob + snap_gap)).abs()));
            y_candidates.push((oy - moving_h - snap_gap, (moving_b - (oy - snap_gap)).abs()));
        }
    }

    let (snap_x, sx_opt) = snap_axis(new_x, &x_candidates, st.snapped_x, snap_dist);
    let (snap_y, sy_opt) = snap_axis(new_y, &y_candidates, st.snapped_y, snap_dist);

    st.snapped_x = sx_opt;
    st.snapped_y = sy_opt;

    (snap_x.max(0), snap_y.max(0))
}

/// Resize snapping
pub fn snap_resize(elements: &[Element], idx: usize, new_w: i32, new_h: i32, snap_distance: i32) -> (i32, i32) {
    let (mut snap_w, mut snap_h) = (new_w, new_h);

    if idx >= elements.len() {
        return (new_w, new_h);
    }

    let moving = &elements[idx].display;
    let moving_r = moving.x + new_w;
    let moving_b = moving.y + new_h;

    for (i, o) in elements.iter().enumerate() {
        if i == idx {
            continue;
        }
        let other = &o.display;
        let ox = other.x;
        let oy = other.y;
        let or = other.x + other.width;
        let ob = other.y + other.height;

        if (moving_r - ox).abs() <= snap_distance { snap_w = ox - moving.x; }
        if (moving_r - or).abs() <= snap_distance { snap_w = or - moving.x; }
        if (moving_b - oy).abs() <= snap_distance { snap_h = oy - moving.y; }
        if (moving_b - ob).abs() <= snap_distance { snap_h = ob - moving.y; }
    }

    (snap_w.max(20), snap_h.max(20))
}

/// Draw snap guide lines
// In the arranging phase, snap coords are overlay-relative and need the overlay offset added to become screen coords;
// in the selecting phase they are already screen coords, so no offset is added.
pub unsafe fn draw_snap_lines(hdc: HDC, st: &EditorState) {
    if !st.snap_on {
        return;
    }
    let (ox, oy) = if st.phase == super::common::EditorPhase::Arranging {
        (st.overlay_x, st.overlay_y)
    } else {
        (0, 0)
    };
    let snap_color = rgb(255, 220, 60);
    if let Some(sx) = st.snapped_x {
        super::common::fill_rect_solid(hdc, sx + ox, 0, 1, st.screen_h, snap_color);
    }
    if let Some(sy) = st.snapped_y {
        super::common::fill_rect_solid(hdc, 0, sy + oy, st.screen_w, 1, snap_color);
    }
}
