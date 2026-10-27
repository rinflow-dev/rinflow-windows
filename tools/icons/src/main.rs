//! Run only when updating the logo. Normal builds use the checked-in assets.
use resvg::{tiny_skia, usvg};
use std::{error::Error, fs, path::Path};

const SIZES: &[u32] = &[16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256];

fn main() -> Result<(), Box<dyn Error>> {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/Assets");
    let source = fs::read_to_string(assets.join("LinkCursor.svg"))?;
    // Preserve the supplied geometry and gradient; lift only the indigo stem
    // on dark surfaces, where the original #1E1B4B would disappear.
    if !source.contains("#1E1B4B") {
        return Err("expected the indigo stem in LinkCursor.svg".into());
    }
    let dark = source.replace("#1E1B4B", "#E0E7FF");
    fs::write(assets.join("LinkCursor.Dark.svg"), &dark)?;
    for (name, svg) in [("LinkCursor", source), ("LinkCursor.Dark", dark)] {
        let tree = usvg::Tree::from_str(&svg, &usvg::Options::default())?;
        let mut images = Vec::new();
        for &size in SIZES {
            let mut pixmap = tiny_skia::Pixmap::new(size, size).ok_or("allocate icon")?;
            let transform = tiny_skia::Transform::from_scale(
                size as f32 / tree.size().width(),
                size as f32 / tree.size().height(),
            );
            resvg::render(&tree, transform, &mut pixmap.as_mut());
            // PNG compression for Explorer's 256px image; 32-bit DIBs for
            // smaller images, including Inno Setup's small-icon reader.
            images.push(if size == 256 {
                pixmap.encode_png()?
            } else {
                dib(&pixmap)
            });
        }
        let mut ico = Vec::from([0, 0, 1, 0]);
        ico.extend_from_slice(&(SIZES.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * SIZES.len() as u32;
        for (&size, data) in SIZES.iter().zip(&images) {
            ico.extend_from_slice(&[size as u8, size as u8, 0, 0]);
            ico.extend_from_slice(&1u16.to_le_bytes());
            ico.extend_from_slice(&32u16.to_le_bytes());
            ico.extend_from_slice(&(data.len() as u32).to_le_bytes());
            ico.extend_from_slice(&offset.to_le_bytes());
            offset += data.len() as u32;
        }
        for data in images {
            ico.extend(data);
        }
        fs::write(assets.join(format!("{name}.ico")), ico)?;
    }
    println!("Generated light/dark ICOs with sizes {SIZES:?}");
    Ok(())
}

fn dib(pixmap: &tiny_skia::Pixmap) -> Vec<u8> {
    let size = pixmap.width() as usize;
    let mask_stride = size.div_ceil(32) * 4;
    let mut data = Vec::new();
    for value in [40u32, size as u32, size as u32 * 2] {
        data.extend_from_slice(&value.to_le_bytes());
    }
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&32u16.to_le_bytes());
    for value in [0u32, (size * size * 4) as u32, 0, 0, 0, 0] {
        data.extend_from_slice(&value.to_le_bytes());
    }
    for y in (0..size).rev() {
        for x in 0..size {
            let p = pixmap.pixels()[y * size + x].demultiply();
            data.extend_from_slice(&[p.blue(), p.green(), p.red(), p.alpha()]);
        }
    }
    for y in (0..size).rev() {
        let mut row = vec![0u8; mask_stride];
        for x in 0..size {
            if pixmap.pixels()[y * size + x].alpha() == 0 {
                row[x / 8] |= 0x80 >> (x % 8);
            }
        }
        data.extend(row);
    }
    data
}
