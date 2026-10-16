//! Product mark: static application branding is purple; tray state colours are independent.
pub const APP_COLOR: (u8, u8, u8) = (99, 91, 154);
pub fn rgba(size: usize, color: (u8, u8, u8)) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(size * size * 4);
    let lines = [
        (32., 10., 50., 17.),
        (50., 17., 50., 32.),
        (50., 32., 44., 45.),
        (44., 45., 32., 55.),
        (32., 55., 20., 45.),
        (20., 45., 14., 32.),
        (14., 32., 14., 17.),
        (14., 17., 32., 10.),
        (22., 25., 39., 25.),
        (39., 25., 34., 20.),
        (42., 39., 25., 39.),
        (25., 39., 30., 44.),
    ];
    for y in 0..size {
        for x in 0..size {
            let mut coverage = 0;
            let mut white = 0;
            for dy in [0.25, 0.75] {
                for dx in [0.25, 0.75] {
                    let (px, py) = (
                        (x as f32 + dx) * 64. / size as f32,
                        (y as f32 + dy) * 64. / size as f32,
                    );
                    let corner_x = (px - 32.).abs() - 16.;
                    let corner_y = (py - 32.).abs() - 16.;
                    if corner_x.max(0.).hypot(corner_y.max(0.)) > 16. {
                        continue;
                    }
                    coverage += 1;
                    if lines.iter().any(|&(ax, ay, bx, by)| {
                        let (vx, vy) = (bx - ax, by - ay);
                        let t =
                            (((px - ax) * vx + (py - ay) * vy) / (vx * vx + vy * vy)).clamp(0., 1.);
                        (px - ax - t * vx).hypot(py - ay - t * vy) <= 1.6
                    }) {
                        white += 1;
                    }
                }
            }
            let mix = |v: u8| {
                (u32::from(v) * (coverage - white) + 255 * white)
                    .checked_div(coverage)
                    .unwrap_or(0) as u8
            };
            pixels.extend([
                mix(color.0),
                mix(color.1),
                mix(color.2),
                (coverage * 255 / 4) as u8,
            ]);
        }
    }
    pixels
}

/// Single 64px uncompressed Windows ICO, generated during the build without external assets.
pub fn ico() -> Vec<u8> {
    let size = 64usize;
    let pixels = rgba(size, APP_COLOR);
    let image_bytes = 40 + size * size * 4 + size * size / 8;
    let mut result = vec![0, 0, 1, 0, 1, 0, 64, 64, 0, 0, 1, 0, 32, 0];
    result.extend((image_bytes as u32).to_le_bytes());
    result.extend(22u32.to_le_bytes());
    result.extend(40u32.to_le_bytes());
    result.extend((size as i32).to_le_bytes());
    result.extend(((size * 2) as i32).to_le_bytes());
    result.extend([1, 0, 32, 0]);
    result.extend([0; 24]);
    for row in (0..size).rev() {
        for col in 0..size {
            let offset = (row * size + col) * 4;
            result.extend([
                pixels[offset + 2],
                pixels[offset + 1],
                pixels[offset],
                pixels[offset + 3],
            ]);
        }
    }
    result.extend(vec![0; size * size / 8]);
    result
}
