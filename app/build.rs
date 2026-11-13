//! Windows: draw the app icon (src/icon.rs, the same code the window and tray
//! use), write it as a multi-size .ico, and embed it so Explorer, shortcuts and
//! the taskbar show the heart for the .exe itself.

#[path = "src/icon.rs"]
#[allow(dead_code)]
mod icon;

use std::path::PathBuf;

const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("tdxcardio.ico");
    std::fs::write(&out, ico(&SIZES)).expect("write tdxcardio.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(out.to_str().unwrap());
    res.set("FileDescription", "TDXCardio: TouchDesigner heartbeat watchdog");
    res.set("ProductName", "TDXCardio");
    res.compile().expect("embed the icon resource (needs rc.exe from the Windows SDK)");
}

/// An .ico with one 32-bit BMP image per size (alpha in the pixels, empty AND mask).
fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&n| bmp(n)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&n, img) in sizes.iter().zip(&images) {
        let dim = if n >= 256 { 0 } else { n as u8 }; // 0 means 256
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        out.extend_from_slice(&img);
    }
    out
}

fn bmp(n: u32) -> Vec<u8> {
    let rgba = icon::heart_rgba(n);
    let mask_row = ((n + 31) / 32) * 4; // 1 bit per pixel, rows padded to 4 bytes
    let pixels = n * n * 4;
    let mut out = Vec::new();
    for v in [40u32, n, n * 2] {
        out.extend_from_slice(&v.to_le_bytes()); // header size, width, height (XOR + AND)
    }
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bit count
    for v in [0u32, pixels + mask_row * n, 0, 0, 0, 0] {
        out.extend_from_slice(&v.to_le_bytes()); // compression, image size, ppm x/y, colours
    }
    for y in (0..n).rev() {
        // bottom-up rows, BGRA
        for x in 0..n {
            let i = ((y * n + x) * 4) as usize;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    out.resize(out.len() + (mask_row * n) as usize, 0);
    out
}
