use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use base64::Engine;
use sonicterm_render_model::InlineImage;
use sonicterm_vt::vt::{MediaEvent, MediaProtocol};

static NEXT_IMAGE_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn decode_inline_image(event: &MediaEvent) -> Option<InlineImage> {
    let (width, height, bgra) = match event.protocol {
        MediaProtocol::Iterm2File | MediaProtocol::Kitty => decode_base64_image(&event.data)?,
        MediaProtocol::Sixel => decode_sixel(&event.data)?,
    };

    Some(InlineImage {
        id: NEXT_IMAGE_ID.fetch_add(1, Ordering::Relaxed),
        row: event.row,
        col: event.col,
        width,
        height,
        bgra: Arc::from(bgra),
    })
}

fn decode_base64_image(encoded: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    let mut image = image::load_from_memory(&bytes).ok()?;
    const MAX_INLINE_IMAGE_SIDE: u32 = 1024;
    if image.width() > MAX_INLINE_IMAGE_SIDE || image.height() > MAX_INLINE_IMAGE_SIDE {
        image = image.resize(
            MAX_INLINE_IMAGE_SIDE,
            MAX_INLINE_IMAGE_SIDE,
            image::imageops::FilterType::Lanczos3,
        );
    }
    let image = image.to_rgba8();
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return None;
    }

    let mut bgra = Vec::with_capacity(width as usize * height as usize * 4);
    for px in image.pixels() {
        let [r, g, b, a] = px.0;
        let alpha = u16::from(a);
        let premul = |channel: u8| ((u16::from(channel) * alpha + 127) / 255) as u8;
        bgra.push(premul(b));
        bgra.push(premul(g));
        bgra.push(premul(r));
        bgra.push(a);
    }

    Some((width, height, bgra))
}

fn decode_sixel(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    const MAX_SIDE: usize = 1024;
    let mut palette = [[0u8, 0, 0, 255]; 256];
    palette[0] = [0, 0, 0, 255];
    palette[1] = [255, 255, 255, 255];

    let mut color_idx = 1usize;
    let mut pixels = vec![0u8; MAX_SIDE * MAX_SIDE * 4];
    let mut x = 0usize;
    let mut y = 0usize;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    let mut repeat = 1usize;
    let mut i = 0usize;

    while i < data.len() {
        match data[i] {
            b'"' => {
                i += 1;
                skip_sixel_params(data, &mut i);
            }
            b'#' => {
                i += 1;
                let idx = parse_sixel_number(data, &mut i).unwrap_or(0).min(255) as usize;
                color_idx = idx;
                if data.get(i) == Some(&b';') {
                    i += 1;
                    let mode = parse_sixel_number(data, &mut i).unwrap_or(0);
                    if data.get(i) == Some(&b';') {
                        i += 1;
                    }
                    let a = parse_sixel_number(data, &mut i).unwrap_or(0);
                    if data.get(i) == Some(&b';') {
                        i += 1;
                    }
                    let b = parse_sixel_number(data, &mut i).unwrap_or(0);
                    if data.get(i) == Some(&b';') {
                        i += 1;
                    }
                    let c = parse_sixel_number(data, &mut i).unwrap_or(0);
                    if mode == 2 {
                        palette[idx] = [percent_to_u8(a), percent_to_u8(b), percent_to_u8(c), 255];
                    }
                }
            }
            b'!' => {
                i += 1;
                repeat = parse_sixel_number(data, &mut i).unwrap_or(1).max(1) as usize;
            }
            b'$' => {
                x = 0;
                i += 1;
            }
            b'-' => {
                x = 0;
                y = y.saturating_add(6);
                i += 1;
            }
            byte @ b'?'..=b'~' => {
                let bits = byte - 63;
                for dx in 0..repeat {
                    let px = x + dx;
                    if px >= MAX_SIDE {
                        continue;
                    }
                    for bit in 0..6 {
                        if bits & (1 << bit) == 0 {
                            continue;
                        }
                        let py = y + bit as usize;
                        if py >= MAX_SIDE {
                            continue;
                        }
                        let off = (py * MAX_SIDE + px) * 4;
                        let [r, g, b, a] = palette[color_idx];
                        pixels[off] = b;
                        pixels[off + 1] = g;
                        pixels[off + 2] = r;
                        pixels[off + 3] = a;
                        max_x = max_x.max(px + 1);
                        max_y = max_y.max(py + 1);
                    }
                }
                x = x.saturating_add(repeat);
                repeat = 1;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    if max_x == 0 || max_y == 0 {
        return None;
    }

    let mut packed = Vec::with_capacity(max_x * max_y * 4);
    for row in 0..max_y {
        let off = row * MAX_SIDE * 4;
        packed.extend_from_slice(&pixels[off..off + max_x * 4]);
    }
    Some((max_x as u32, max_y as u32, packed))
}

fn skip_sixel_params(data: &[u8], i: &mut usize) {
    while *i < data.len() {
        match data[*i] {
            b'0'..=b'9' | b';' => *i += 1,
            _ => break,
        }
    }
}

fn parse_sixel_number(data: &[u8], i: &mut usize) -> Option<u32> {
    let start = *i;
    let mut value = 0u32;
    while *i < data.len() {
        match data[*i] {
            b'0'..=b'9' => {
                value = value.saturating_mul(10).saturating_add(u32::from(data[*i] - b'0'));
                *i += 1;
            }
            _ => break,
        }
    }
    (*i > start).then_some(value)
}

fn percent_to_u8(v: u32) -> u8 {
    ((v.min(100) * 255 + 50) / 100) as u8
}
