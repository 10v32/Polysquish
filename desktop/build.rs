//! Build script for the desktop app.
//!
//! Generates the application icon procedurally (a pink-to-purple blob in the spirit of the
//! web UI logo) so no binary assets need to live in the repository:
//!
//! * `desktop/icons/icon.png`  512x512 RGBA, used by the Linux `.desktop` entry, the AppImage
//!   and (via `iconutil`) the macOS `.icns`.
//! * `desktop/icons/icon.ico`  multi-size ICO, embedded into the Windows executable.
//! * `$OUT_DIR/icon-256.rgba`  raw RGBA pixels used for the runtime window icon.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use image::{Rgba, RgbaImage};

const SIZE: u32 = 512;
/// `#ff5fd2` (top-left of the gradient).
const PINK: [f32; 3] = [255.0, 95.0, 210.0];
/// `#8b5cf6` (bottom-right of the gradient).
const PURPLE: [f32; 3] = [139.0, 92.0, 246.0];
const ICO_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];

fn main() {
    // Only rerun when this script changes; otherwise the icons we write into the source tree
    // would retrigger the build script on every invocation.
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let icons_dir = manifest_dir.join("icons");
    fs::create_dir_all(&icons_dir).expect("create desktop/icons");

    let master = render_icon(SIZE);
    master.save(icons_dir.join("icon.png")).expect("write icons/icon.png");
    write_ico(&master, &icons_dir.join("icon.ico"));

    let window_icon = image::imageops::resize(&master, 256, 256, FilterType::Lanczos3);
    fs::write(out_dir.join("icon-256.rgba"), window_icon.as_raw()).expect("write icon-256.rgba");

    embed_windows_resources(&icons_dir, &out_dir);
}

/// Smoothstep-style clamp used for anti-aliasing and soft gradients.
fn smooth(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Radius of the blob outline at polar angle `theta` (radians), in units of the base radius.
fn blob_radius(theta: f32) -> f32 {
    1.0 + 0.07 * (2.0 * theta + 0.9).sin() + 0.035 * (3.0 * theta - 0.6).sin()
}

fn render_icon(size: u32) -> RgbaImage {
    let s = size as f32;
    let center = s * 0.5;
    let base_radius = s * 0.42;
    // Width of the anti-aliasing band in pixels.
    let aa = 1.5;
    let mut img = RgbaImage::new(size, size);

    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let dx = px - center;
            let dy = py - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let theta = dy.atan2(dx);
            let edge = base_radius * blob_radius(theta);
            let coverage = 1.0 - smooth(edge - aa, edge + aa, dist);
            if coverage <= 0.0 {
                continue;
            }

            // Diagonal gradient, pink top-left to purple bottom-right (matches the UI logo).
            let t = ((px + py) / (2.0 * s)).clamp(0.0, 1.0);
            let mut rgb = [0.0f32; 3];
            for i in 0..3 {
                rgb[i] = PINK[i] + (PURPLE[i] - PINK[i]) * t;
            }

            // Soft radial shine towards the upper left, like the `pg-shine` gradient.
            let sx = (px / s - 0.3) / 0.8;
            let sy = (py / s - 0.25) / 0.8;
            let shine = 0.55 * (1.0 - smooth(0.0, 0.6, (sx * sx + sy * sy).sqrt()));
            for c in rgb.iter_mut() {
                *c += (255.0 - *c) * shine;
            }

            // Small white highlight dot in the upper right.
            let hx = px - s * (33.0 / 48.0);
            let hy = py - s * (15.0 / 48.0);
            let dot_r = s * (3.2 / 48.0);
            let dot = 0.9 * (1.0 - smooth(dot_r - aa, dot_r + aa, (hx * hx + hy * hy).sqrt()));
            for c in rgb.iter_mut() {
                *c += (255.0 - *c) * dot;
            }

            img.put_pixel(
                x,
                y,
                Rgba([
                    rgb[0].round().clamp(0.0, 255.0) as u8,
                    rgb[1].round().clamp(0.0, 255.0) as u8,
                    rgb[2].round().clamp(0.0, 255.0) as u8,
                    (coverage * 255.0).round() as u8,
                ]),
            );
        }
    }
    img
}

fn write_ico(master: &RgbaImage, path: &Path) {
    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &sz in ICO_SIZES {
        let resized = image::imageops::resize(master, sz, sz, FilterType::Lanczos3);
        let icon = ico::IconImage::from_rgba_data(sz, sz, resized.into_raw());
        let entry = ico::IconDirEntry::encode(&icon).expect("encode ICO entry");
        dir.add_entry(entry);
    }
    let file = fs::File::create(path).expect("create icons/icon.ico");
    dir.write(file).expect("write icons/icon.ico");
}

/// Embed the icon into the Windows executable. The `embed-resource` crate is only a dependency
/// when building on Windows; the target check keeps cross-compilation sane.
#[cfg(windows)]
fn embed_windows_resources(icons_dir: &Path, out_dir: &Path) {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ico = out_dir.join("icon.ico");
    fs::copy(icons_dir.join("icon.ico"), &ico).expect("copy icon.ico to OUT_DIR");
    // rc.exe accepts forward slashes in quoted paths and OUT_DIR is also passed as an include dir.
    let ico_str = ico.to_string_lossy().replace('\\', "/");
    let rc = out_dir.join("polysquish.rc");
    fs::write(&rc, format!("1 ICON \"{ico_str}\"\n")).expect("write polysquish.rc");
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_optional()
        .expect("compile Windows resources (rc.exe / windres)");
}

#[cfg(not(windows))]
fn embed_windows_resources(_icons_dir: &Path, _out_dir: &Path) {}
