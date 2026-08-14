//! APortal program icon: a "portal" icon rasterized purely in software
//!
//! Design: dark blue outer ring + inner cyan glow + bright center core.
//! Outputs BGRA premultiplied alpha pixels, usable for:
//!   1. gen_icon to generate .ico (multi-size PNG)
//!   2. tray HICON (CreateIconIndirect)
//!
//! Parameterized: any integer size works (16/32/48/256...), 2x2 supersampling anti-aliasing.

/// Render the APortal icon at the given size, returning premultiplied BGRA pixels (w*h*4).
pub fn render_portal(size: u32) -> Vec<u8> {
    let s = size as f64;
    let center = s / 2.0;

    // Geometry params (ratios of the size)
    let ring_outer = s * 0.41; // outer ring radius
    let ring_inner = s * 0.27; // inner ring radius
    let ring_width = ring_outer - ring_inner;
    let core_r = s * 0.055; // center bright core
    let gap_for_arms = ring_inner * 0.92; // spiral arm reach

    let mut buf = vec![0u8; (size * size * 4) as usize];

    for y in 0..size {
        for x in 0..size {
            let mut acc = [0.0f64; 4];
            // 2x2 supersampling anti-aliasing
            for sy in 0..2usize {
                for sx in 0..2usize {
                    let px = x as f64 + sx as f64 * 0.5 + 0.25;
                    let py = y as f64 + sy as f64 * 0.5 + 0.25;
                    sample(px, py, center, ring_outer, ring_inner, ring_width, core_r, gap_for_arms, &mut acc);
                }
            }
            let n = 4.0f64;
            let idx = ((y * size + x) * 4) as usize;
            buf[idx] = (acc[0] / n * 255.0).clamp(0.0, 255.0) as u8; // B
            buf[idx + 1] = (acc[1] / n * 255.0).clamp(0.0, 255.0) as u8; // G
            buf[idx + 2] = (acc[2] / n * 255.0).clamp(0.0, 255.0) as u8; // R
            buf[idx + 3] = (acc[3] / n * 255.0).clamp(0.0, 255.0) as u8; // A
        }
    }
    buf
}

/// Single-point sampling: accumulate the point's color into acc (acc holds BGRA, premultiplied alpha)
#[allow(clippy::too_many_arguments)]
fn sample(
    px: f64,
    py: f64,
    center: f64,
    ring_outer: f64,
    ring_inner: f64,
    ring_width: f64,
    core_r: f64,
    gap: f64,
    acc: &mut [f64; 4],
) {
    let dx = px - center;
    let dy = py - center;
    let dist = (dx * dx + dy * dy).sqrt();
    let ang = dy.atan2(dx); // -pi..pi

    // ===== Center core: near-white cyan, solid disk + penumbra
    if dist <= core_r {
        acc[0] += 0.75;
        acc[1] += 0.9;
        acc[2] += 1.0;
        acc[3] += 1.0;
        return;
    }

    // ===== Ring: bright cyan outside, dark blue inside, radial gradient
    if dist <= ring_outer && dist >= ring_inner {
        // t: inside→outside 0..1
        let t = ((dist - ring_inner) / ring_width).clamp(0.0, 1.0);
        let (r, g, b) = ring_gradient(t);
        // 1px soft edge on both sides (supersampling already provides AA; this adds a 1px gradient at the ring edges)
        let edge = (dist - ring_inner).min(ring_outer - dist);
        let a = (edge * 2.0).clamp(0.0, 1.0);
        add_premul(acc, r, g, b, a);
        return;
    }

    // ===== Inner space: three spiral arms (only far enough from the core)
    if dist > core_r && dist <= gap {
        // Spiral arm: r(ang) grows with the angle (Archimedean spiral)
        // Three arms, each normalized to 0..1
        let mut a3 = (ang + std::f64::consts::PI) / (std::f64::consts::PI * 2.0); // 0..1
        a3 *= 3.0; // 3 arms
        let arm_phase = a3 - a3.floor(); // 0..1
        // Arm center sits at phase 0.5
        let _arm_dist = (arm_phase - 0.5).abs() * 2.0; // 0..1, 0 at center
        // Arm radial position: more spiral-like toward the outside
        let spiral_r = gap * (0.55 + 0.45 * arm_phase);
        let r_delta = (dist - spiral_r).abs() / (gap * 0.09);
        let w = (1.0 - r_delta).clamp(0.0, 1.0);
        if w > 0.0 {
            let glow = (1.0 - dist / gap) * 0.6 + 0.4;
            let bc = 0.35 * glow;
            let gc = 0.9 * glow;
            let rc = 1.0 * glow;
            add_premul(acc, rc, gc, bc, w * 0.7);
        }
        // No return here: let the faint cyan base layer fill in, otherwise the ring interior would be hollow
    }

    // ===== Ring base color: very faint cyan (for a sense of depth)
    if dist > core_r && dist <= ring_inner {
        let fade = (ring_inner - dist) / (ring_inner - core_r);
        let rec = 0.05 * fade;
        add_premul(acc, rec, 0.18 * fade + 0.02, 0.32 * fade + 0.04, 1.0);
    }
}

/// Ring gradient: inside = dark blue (0.05,0.12,0.35), outside = bright cyan (0.15,0.85,1.0)
fn ring_gradient(t: f64) -> (f64, f64, f64) {
    let u = t.clamp(0.0, 1.0);
    let r = 0.05 + (0.15 - 0.05) * u;
    let g = 0.12 + (0.85 - 0.12) * u;
    let b = 0.35 + (1.0 - 0.35) * u;
    (r, g, b)
}

/// Accumulate a non-premultiplied color (converted to premultiplied internally)
fn add_premul(acc: &mut [f64; 4], r: f64, g: f64, b: f64, a: f64) {
    let a = a.clamp(0.0, 1.0);
    acc[0] += b * a;
    acc[1] += g * a;
    acc[2] += r * a;
    acc[3] += a;
}