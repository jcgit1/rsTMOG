//! Headless capture (`rstmog --capture [page] [out.png]`): runs the real
//! page code through `egui::Context::run` + `tessellate` (no window, no
//! GPU) and rasterizes the mesh triangles in software — implementing the
//! egui_glow shader contract exactly: `frag = vertex_color × texel`, both
//! premultiplied RGBA in gamma space, composited over an opaque frame.
//! Font glyphs live in the texture atlas (Managed(0)) so text renders
//! correctly without a windowing system. Good enough for review and CI
//! artifacts; not a product renderer.

use crate::gui::App;
use eframe::egui;
use egui::epaint::TextureId;

pub fn capture(app: &mut App, page: crate::gui::Page, out: &std::path::Path) -> anyhow::Result<()> {
    let scale = 1.0f32;
    let (w, h) = (1100u32, 700u32);
    let ctx = egui::Context::default();
    ctx.set_visuals(egui::Visuals::dark());
    let mut raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(w as f32, h as f32),
        )),
        time: Some(0.0),
        ..Default::default()
    };
    raw.viewports.insert(
        egui::ViewportId::ROOT,
        egui::ViewportInfo {
            native_pixels_per_point: Some(scale),
            ..Default::default()
        },
    );
    let full_output = ctx.run(raw, |ctx| {
        crate::gui::render_page(ctx, app, page);
    });
    let clipped = ctx.tessellate(full_output.shapes, full_output.pixels_per_point);

    // full atlas images the backend would upload this pass (Managed(0) =
    // the font atlas; solid shapes sample the white pixel at uv (0,0))
    let mut textures: std::collections::HashMap<u64, (usize, usize, Vec<u8>)> = Default::default();
    for (id, delta) in full_output.textures_delta.set.iter() {
        let egui::epaint::ImageData::Color(ci) = &delta.image;
        let rgba: Vec<u8> = ci.pixels.iter().flat_map(|c| c.to_array()).collect();
        textures.insert(id.index(), (ci.width(), ci.height(), rgba));
    }
    if !textures.contains_key(&0) {
        return Err(anyhow::anyhow!(
            "font atlas (Managed(0)) missing from texture deltas"
        ));
    }

    let fb = render_clipped(w, h, &clipped, &textures);
    write_png(out, w, h, &fb)?;
    Ok(())
}

trait IdIndex {
    fn index(&self) -> u64;
}
impl IdIndex for TextureId {
    fn index(&self) -> u64 {
        match self {
            TextureId::Managed(i) => *i,
            TextureId::User(i) => *i,
        }
    }
}

/// Fill bg and paint every clipped mesh triangle.
fn render_clipped(
    w: u32,
    h: u32,
    clipped: &[egui::epaint::ClippedPrimitive],
    textures: &std::collections::HashMap<u64, (usize, usize, Vec<u8>)>,
) -> Vec<u8> {
    let mut fb = vec![0u8; w as usize * h as usize * 4];
    const BG: [u8; 4] = [24, 26, 31, 255];
    for chunk in fb.chunks_mut(4) {
        chunk.copy_from_slice(&BG);
    }
    for cp in clipped {
        let egui::epaint::Primitive::Mesh(mesh) = &cp.primitive else {
            continue;
        };
        let tex = match mesh.texture_id {
            TextureId::Managed(i) => textures.get(&i),
            _ => None,
        };
        for idx in mesh.indices.chunks(3) {
            let v = [
                &mesh.vertices[idx[0] as usize],
                &mesh.vertices[idx[1] as usize],
                &mesh.vertices[idx[2] as usize],
            ];
            raster_tri(&mut fb, w, h, cp.clip_rect, v, tex);
        }
    }
    fb
}

#[inline]
fn raster_tri(
    fb: &mut [u8],
    w: u32,
    h: u32,
    clip: egui::Rect,
    v: [&egui::epaint::Vertex; 3],
    tex: Option<&(usize, usize, Vec<u8>)>,
) {
    let min_x = v[0]
        .pos
        .x
        .min(v[1].pos.x)
        .min(v[2].pos.x)
        .max(clip.min.x)
        .max(0.0) as i32;
    let max_x = v[0]
        .pos
        .x
        .max(v[1].pos.x)
        .max(v[2].pos.x)
        .min(clip.max.x)
        .min(w as f32) as i32;
    let min_y = v[0]
        .pos
        .y
        .min(v[1].pos.y)
        .min(v[2].pos.y)
        .max(clip.min.y)
        .max(0.0) as i32;
    let max_y = v[0]
        .pos
        .y
        .max(v[1].pos.y)
        .max(v[2].pos.y)
        .min(clip.max.y)
        .min(h as f32) as i32;
    if min_x >= max_x || min_y >= max_y {
        return;
    }
    let (x0, y0) = (v[0].pos.x as f64, v[0].pos.y as f64);
    let (x1, y1) = (v[1].pos.x as f64, v[1].pos.y as f64);
    let (x2, y2) = (v[2].pos.x as f64, v[2].pos.y as f64);
    let denom = (y1 - y2) * (x0 - x2) + (x2 - x1) * (y0 - y2);
    if denom.abs() < 1e-9 {
        return;
    }
    let inv = 1.0 / denom;
    let (tw, th, rgba) = tex
        .map(|t| (t.0 as f64, t.1 as f64, t.2.as_slice()))
        .unwrap_or((1.0, 1.0, &[255, 255, 255, 255][..]));
    for y in min_y..max_y {
        for x in min_x..max_x {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let b0 = ((y1 - y2) * (px - x2) + (x2 - x1) * (py - y2)) * inv;
            let b1 = ((y2 - y0) * (px - x2) + (x0 - x2) * (py - y2)) * inv;
            let b2 = 1.0 - b0 - b1;
            if b0 < -0.001 || b1 < -0.001 || b2 < -0.001 {
                continue;
            }
            let col = [
                b0 * v[0].color[0] as f64 + b1 * v[1].color[0] as f64 + b2 * v[2].color[0] as f64,
                b0 * v[0].color[1] as f64 + b1 * v[1].color[1] as f64 + b2 * v[2].color[1] as f64,
                b0 * v[0].color[2] as f64 + b1 * v[1].color[2] as f64 + b2 * v[2].color[2] as f64,
                b0 * v[0].color[3] as f64 + b1 * v[1].color[3] as f64 + b2 * v[2].color[3] as f64,
            ];
            let u = b0 * v[0].uv.x as f64 + b1 * v[1].uv.x as f64 + b2 * v[2].uv.x as f64;
            let vv = b0 * v[0].uv.y as f64 + b1 * v[1].uv.y as f64 + b2 * v[2].uv.y as f64;
            let tx = (u * tw) as i64;
            let ty = (vv * th) as i64;
            if tx < 0 || ty < 0 || tx >= tw as i64 || ty >= th as i64 {
                continue;
            }
            let to = (ty as usize * tw as usize + tx as usize) * 4;
            let (tr, tg, tb, ta) = if to + 3 < rgba.len() {
                (
                    rgba[to] as f64,
                    rgba[to + 1] as f64,
                    rgba[to + 2] as f64,
                    rgba[to + 3] as f64,
                )
            } else {
                (255.0, 255.0, 255.0, 255.0)
            };
            // frag = vertex_color * texel (componentwise, premultiplied),
            // over opaque dst: out = frag_rgb + dst_rgb * (1 - frag_a)
            let fa = (col[3] / 255.0) * (ta / 255.0);
            if fa <= 0.002 {
                continue;
            }
            let fr = (col[0] / 255.0) * (tr / 255.0);
            let fg = (col[1] / 255.0) * (tg / 255.0);
            let fbr = (col[2] / 255.0) * (tb / 255.0);
            let o = ((y as u32 * w + x as u32) * 4) as usize;
            fb[o] = (fr * 255.0 + fb[o] as f64 * (1.0 - fa)).clamp(0.0, 255.0) as u8;
            fb[o + 1] = (fg * 255.0 + fb[o + 1] as f64 * (1.0 - fa)).clamp(0.0, 255.0) as u8;
            fb[o + 2] = (fbr * 255.0 + fb[o + 2] as f64 * (1.0 - fa)).clamp(0.0, 255.0) as u8;
            fb[o + 3] = 255;
        }
    }
}

/// Minimal PNG writer (no deps): stored (uncompressed) deflate blocks.
fn write_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut rows = Vec::with_capacity(h as usize * (1 + w as usize * 4));
    for y in 0..h as usize {
        rows.push(0u8); // filter: none
        let start = y * (w as usize * 4);
        rows.extend_from_slice(&rgba[start..start + w as usize * 4]);
    }
    let mut zlib = Vec::with_capacity(rows.len() + 16);
    zlib.extend_from_slice(&[0x78, 0x01]); // CMF/FLG, no preset dict
    let mut i = 0;
    while i < rows.len() {
        let n = (rows.len() - i).min(65535);
        let last = i + n >= rows.len();
        zlib.push(if last { 1 } else { 0 });
        zlib.extend_from_slice(&(n as u16).to_le_bytes());
        zlib.extend_from_slice(&(n as u16 ^ 0xFFFF).to_le_bytes());
        zlib.extend_from_slice(&rows[i..i + n]);
        i += n;
    }
    let mut s1: u32 = 1;
    let mut s2: u32 = 0;
    for b in &rows {
        s1 = (s1 + *b as u32) % 65521;
        s2 = (s2 + s1) % 65521;
    }
    zlib.extend_from_slice(&((s2 << 16) | s1).to_be_bytes());

    let mut out = Vec::with_capacity(zlib.len() + 64);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(&[kind.as_slice(), data].concat()).to_be_bytes());
    }
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", b"");

    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(&out)?;
    Ok(())
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB88320 & (!(crc & 1)));
        }
    }
    !crc
}
