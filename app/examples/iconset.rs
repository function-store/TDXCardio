//! Writes the app icon as raw RGBA files, one per size, for packaging
//! (the macOS workflow turns them into PNGs and a .icns).
//!
//! cargo run --release --example iconset -- <out_dir>
//! -> <out_dir>/icon_<n>.rgba for n in 16, 32, 64, 128, 256, 512, 1024

#[path = "../src/icon.rs"]
mod icon;

fn main() {
    let out = std::env::args().nth(1).expect("usage: iconset <out_dir>");
    std::fs::create_dir_all(&out).expect("create out dir");
    for n in [16u32, 32, 64, 128, 256, 512, 1024] {
        let path = std::path::Path::new(&out).join(format!("icon_{n}.rgba"));
        std::fs::write(&path, icon::heart_rgba(n)).expect("write icon");
        println!("{}", path.display());
    }
}
