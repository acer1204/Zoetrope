//! 程式化產生的應用程式圖示（夜色天空、夕陽與遠山），
//! 供視窗圖示（main.rs）與 .ico 產生器（examples/gen_icon.rs）共用。

/// 以 64×64 座標系描述、可縮放到任意尺寸的 RGBA 圖示。
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let s = size as f32 / 64.0;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            // 轉回 64 座標空間套用同一組幾何
            let fx = (x as f32 + 0.5) / s;
            let fy = (y as f32 + 0.5) / s;
            if !inside_rounded(fx, fy) {
                continue;
            }
            let t = fy / 64.0;
            let mut c = (
                (28.0 + 26.0 * t) as u8,
                (32.0 + 28.0 * t) as u8,
                (46.0 + 30.0 * t) as u8,
            );
            let (dx, dy) = (fx - 42.0, fy - 22.0);
            if dx * dx + dy * dy < 64.0 {
                c = (255, 176, 64);
            }
            let m_far = fy > 38.0 + (fx - 46.0).abs() * 0.95;
            let m_near = fy > 30.0 + (fx - 22.0).abs() * 0.85;
            if m_near {
                c = (46, 96, 104);
            } else if m_far {
                c = (60, 120, 124);
            }
            let i = ((y * size + x) * 4) as usize;
            rgba[i] = c.0;
            rgba[i + 1] = c.1;
            rgba[i + 2] = c.2;
            rgba[i + 3] = 255;
        }
    }
    rgba
}

fn inside_rounded(x: f32, y: f32) -> bool {
    const R: f32 = 12.0;
    const S: f32 = 64.0;
    let (cx, cy) = (x.clamp(R, S - R), y.clamp(R, S - R));
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= R * R
}

#[cfg(test)]
mod tests {
    use super::icon_rgba;

    #[test]
    fn sizes_and_corners() {
        for size in [16u32, 64, 256] {
            let px = icon_rgba(size);
            assert_eq!(px.len(), (size * size * 4) as usize);
            // 角落在圓角外 → 透明；中心不透明
            assert_eq!(px[3], 0, "左上角應透明 (size={size})");
            let c = ((size / 2 * size + size / 2) * 4 + 3) as usize;
            assert_eq!(px[c], 255, "中心應不透明 (size={size})");
        }
    }
}
