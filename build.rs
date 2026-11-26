use std::fs;
use std::path::{Path, PathBuf};

fn render_icon_bgra(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut pixels = vec![0u8; (size * size * 4) as usize];

    for y in 0..size {
        for x in 0..size {
            let fx = (x as f32 + 0.5) / s;
            let fy = (y as f32 + 0.5) / s;

            // Rounded square badge background with blue->cyan gradient
            let margin = 0.06;
            let radius = 0.22;
            let dx = (fx - 0.5).abs() - (0.5 - margin - radius);
            let dy = (fy - 0.5).abs() - (0.5 - margin - radius);
            let dist = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - radius;

            let aa = (0.5 / s).max(0.004);
            let bg_alpha = ((0.0 - dist) / (2.0 * aa) + 0.5).clamp(0.0, 1.0);
            if bg_alpha <= 0.001 {
                continue;
            }

            let t = (fx * 0.6 + fy * 0.4).clamp(0.0, 1.0);
            let mut r = 24.0 + t * 20.0;
            let mut g = 112.0 + t * 76.0;
            let mut b = 238.0 + t * 14.0;
            let mut a = bg_alpha;

            // Inner white screenshot crop frame corners (clean geometric brackets)
            let stroke = (0.085_f32).max(1.5 / s);
            let in_outer = fx >= 0.20 && fx <= 0.80 && fy >= 0.20 && fy <= 0.80;
            let in_inner = fx >= 0.20 + stroke
                && fx <= 0.80 - stroke
                && fy >= 0.20 + stroke
                && fy <= 0.80 - stroke;
            let near_corner_x = fx <= 0.42 || fx >= 0.58;
            let near_corner_y = fy <= 0.42 || fy >= 0.58;
            if in_outer && !in_inner && (near_corner_x && near_corner_y) {
                r = 255.0;
                g = 255.0;
                b = 255.0;
                a = a.max(bg_alpha);
            }

            // Center pushpin dot & accent ring
            let cx = fx - 0.50;
            let cy = fy - 0.48;
            let cd = cx.hypot(cy);
            if cd <= 0.145 {
                let pin_aa = ((0.145 - cd) / (2.0 * aa) + 0.5).clamp(0.0, 1.0);
                r = r * (1.0 - pin_aa) + 255.0 * pin_aa;
                g = g * (1.0 - pin_aa) + 255.0 * pin_aa;
                b = b * (1.0 - pin_aa) + 255.0 * pin_aa;
            }
            // Pin needle pointing down-right
            if fx >= 0.50 && fx <= 0.68 && fy >= 0.48 && fy <= 0.68 {
                let line_dist = ((fx - 0.50) - (fy - 0.48)).abs() * 0.7071;
                if line_dist <= stroke * 0.48 {
                    r = 255.0;
                    g = 255.0;
                    b = 255.0;
                }
            }

            let idx = ((y * size + x) * 4) as usize;
            pixels[idx] = b.clamp(0.0, 255.0) as u8;
            pixels[idx + 1] = g.clamp(0.0, 255.0) as u8;
            pixels[idx + 2] = r.clamp(0.0, 255.0) as u8;
            pixels[idx + 3] = (a * 255.0).clamp(0.0, 255.0) as u8;
        }
    }
    pixels
}

fn build_bmp_ico_entry(size: u32) -> Vec<u8> {
    let bgra = render_icon_bgra(size);
    let xor_size = (size * size * 4) as usize;
    let mask_stride = (size.div_ceil(32) * 4) as usize;
    let and_size = mask_stride * size as usize;
    let mut dib = Vec::with_capacity(40 + xor_size + and_size);

    // BITMAPINFOHEADER (40 bytes)
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&(size as i32).to_le_bytes());
    dib.extend_from_slice(&((size * 2) as i32).to_le_bytes()); // height * 2 for XOR + AND masks
    dib.extend_from_slice(&1u16.to_le_bytes()); // planes
    dib.extend_from_slice(&32u16.to_le_bytes()); // bit count
    dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    dib.extend_from_slice(&((xor_size + and_size) as u32).to_le_bytes());
    dib.extend_from_slice(&0i32.to_le_bytes());
    dib.extend_from_slice(&0i32.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());

    // Bottom-up BGRA pixels
    for y in (0..size).rev() {
        let start = (y * size * 4) as usize;
        dib.extend_from_slice(&bgra[start..start + (size * 4) as usize]);
    }
    // AND mask (all 0 since 32-bit alpha channel is used)
    dib.resize(dib.len() + and_size, 0);
    dib
}

fn generate_ico(path: &Path) {
    let sizes = [16u32, 32, 48, 64];
    let entries: Vec<(u32, Vec<u8>)> = sizes
        .iter()
        .map(|&sz| (sz, build_bmp_ico_entry(sz)))
        .collect();

    let mut ico = Vec::new();
    // ICONDIR
    ico.extend_from_slice(&0u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes()); // ICO type
    ico.extend_from_slice(&(entries.len() as u16).to_le_bytes());

    let mut offset = (6 + entries.len() * 16) as u32;
    for (sz, data) in &entries {
        let dim = if *sz >= 256 { 0u8 } else { *sz as u8 };
        ico.push(dim);
        ico.push(dim);
        ico.push(0); // color count
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bpp
        ico.extend_from_slice(&(data.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in entries {
        ico.extend_from_slice(&data);
    }

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(path, ico).expect("write generated rhpicture.ico");
}

fn main() {
    println!("cargo:rerun-if-changed=assets/rhpicture.manifest");
    println!("cargo:rerun-if-changed=assets/rhpicture.ico");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

        let asset_ico = manifest_dir.join("assets").join("rhpicture.ico");
        if !asset_ico.exists() {
            generate_ico(&asset_ico);
        }

        let ico_path = asset_ico
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
            .trim_start_matches("\\\\?\\")
            .replace('\\', "/");
        let manifest_path = manifest_dir
            .join("assets")
            .join("rhpicture.manifest")
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
            .trim_start_matches("\\\\?\\")
            .replace('\\', "/");

        let rc_path = out_dir.join("rhpicture.rc");
        let rc_content = format!(
            "1 ICON \"{ico_path}\"\n1 24 \"{manifest_path}\"\n"
        );
        fs::write(&rc_path, rc_content).expect("write rhpicture.rc");

        embed_resource::compile(&rc_path, embed_resource::NONE)
            .manifest_optional()
            .expect("embed Windows icon and manifest resource");
    }
}
