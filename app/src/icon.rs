//! The app icon, shared by the window, the tray and (via build.rs) the .exe.
//! Plain Rust, no dependencies: build.rs includes this file directly.

/// The app icon: a red heart with a banana inside, drawn procedurally (4x4
/// supersampled) so the binary needs no asset files.
pub fn heart_rgba(n: u32) -> Vec<u8> {
    const SS: u32 = 4;
    let mut out = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let mut acc = [0u32; 4];
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = (x as f32 + (sx as f32 + 0.5) / SS as f32) / n as f32 * 2.6 - 1.3;
                    let fy = 1.32 - (y as f32 + (sy as f32 + 0.5) / SS as f32) / n as f32 * 2.5;
                    if let Some(c) = icon_color(fx, fy) {
                        acc[0] += c[0] as u32;
                        acc[1] += c[1] as u32;
                        acc[2] += c[2] as u32;
                        acc[3] += 1;
                    }
                }
            }
            if acc[3] == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let k = acc[3];
                let alpha = (k * 255 / (SS * SS)) as u8;
                out.extend_from_slice(&[(acc[0] / k) as u8, (acc[1] / k) as u8, (acc[2] / k) as u8, alpha]);
            }
        }
    }
    out
}

/// Colour at heart space (x, y), y up: the heart, or the banana lying
/// diagonally across it (a tapered arc with a stem and a dark tip).
fn icon_color(x: f32, y: f32) -> Option<[u8; 3]> {
    const RED: [u8; 3] = [220, 40, 60];
    const YELLOW: [u8; 3] = [250, 214, 64];
    const SHADE: [u8; 3] = [214, 168, 38];
    const TIP: [u8; 3] = [92, 62, 30];
    let a = x * x + y * y - 1.0;
    if a * a * a - x * x * y * y * y > 0.0 {
        return None;
    }
    let ang = 32f32.to_radians();
    let (px, py) = (x, y - 0.02);
    let lx = px * ang.cos() + py * ang.sin();
    let ly = -px * ang.sin() + py * ang.cos();
    let r = 0.62;
    let (dx, dy) = (lx, ly - r);
    let d = (dx * dx + dy * dy).sqrt();
    let th = dy.atan2(dx).to_degrees().rem_euclid(360.0);
    let (t0, t1) = (196.0, 344.0);
    if th < t0 - 8.0 || th > t1 {
        return Some(RED);
    }
    if th < t0 {
        // the stem, a short stub past the left end
        return Some(if (d - r + 0.04).abs() < 0.07 { TIP } else { RED });
    }
    let t = (th - t0) / (t1 - t0);
    let w = 0.30 * (std::f32::consts::PI * t).sin().powf(0.55);
    let off = d - r;
    if off.abs() > w {
        return Some(RED);
    }
    if t > 0.955 {
        return Some(TIP);
    }
    Some(if off < -w * 0.30 { SHADE } else { YELLOW })
}
