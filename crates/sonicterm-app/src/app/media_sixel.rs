//! Sixel decoding into a buffer sized to the painted extent.
//!
//! A payload is walked twice. The first walk finds the rectangle its set bits reach, clipped to the
//! render-side cap; the second paints into a buffer of exactly that size. A small image therefore
//! allocates only its own pixels, never the cap-sized square, and needs no packing copy.

/// Pixels per side a Sixel payload can address: the same rendered-side cap the base64 path
/// resizes to. Anything painted beyond it is clipped.
pub(super) const MAX_SIDE: usize = super::MAX_INLINE_IMAGE_RENDER_SIDE as usize;

/// One data byte's paint: `repeat` columns from `column`, the six rows from `band_top` selected by
/// `bits`, in `color` (RGBA).
struct SixelRun {
    column: usize,
    band_top: usize,
    repeat: usize,
    bits: u8,
    color: [u8; 4],
}

/// Walk `data`, keeping the palette, cursor and repeat state, and hand every data byte's run to
/// `paint`. Both decode walks share this, so they see the same runs in the same order.
fn walk_sixel(data: &[u8], mut paint: impl FnMut(&SixelRun)) {
    let mut palette = [[0u8, 0, 0, 255]; 256];
    palette[1] = [255, 255, 255, 255];
    let mut color_idx = 1usize;
    let mut cursor_x = 0usize;
    let mut cursor_y = 0usize;
    let mut repeat = 1usize;
    let mut position = 0usize;
    while position < data.len() {
        match data[position] {
            b'"' => {
                position += 1;
                skip_sixel_params(data, &mut position);
            }
            b'#' => {
                position += 1;
                let idx = parse_sixel_number(data, &mut position).unwrap_or(0).min(255) as usize;
                color_idx = idx;
                if data.get(position) == Some(&b';') {
                    position += 1;
                    let mode = parse_sixel_number(data, &mut position).unwrap_or(0);
                    if data.get(position) == Some(&b';') {
                        position += 1;
                    }
                    let red_percent = parse_sixel_number(data, &mut position).unwrap_or(0);
                    if data.get(position) == Some(&b';') {
                        position += 1;
                    }
                    let green_percent = parse_sixel_number(data, &mut position).unwrap_or(0);
                    if data.get(position) == Some(&b';') {
                        position += 1;
                    }
                    let blue_percent = parse_sixel_number(data, &mut position).unwrap_or(0);
                    if mode == 2 {
                        palette[idx] = [
                            percent_to_u8(red_percent),
                            percent_to_u8(green_percent),
                            percent_to_u8(blue_percent),
                            255,
                        ];
                    }
                }
            }
            b'!' => {
                position += 1;
                // Clamped to the raster width: every column past `MAX_SIDE` is clipped, so the
                // clamp changes how long a run takes, not what it paints. Without it a twelve-byte
                // payload (`!4294967295m`) would ask for billions of columns.
                repeat = (parse_sixel_number(data, &mut position).unwrap_or(1).max(1) as usize)
                    .min(MAX_SIDE);
            }
            b'$' => {
                cursor_x = 0;
                position += 1;
            }
            b'-' => {
                cursor_x = 0;
                cursor_y = cursor_y.saturating_add(6);
                position += 1;
            }
            byte @ b'?'..=b'~' => {
                // A data byte: its low six bits paint one column strip
                // of six vertical pixels, `repeat` times.
                paint(&SixelRun {
                    column: cursor_x,
                    band_top: cursor_y,
                    repeat,
                    bits: byte - 63,
                    color: palette[color_idx],
                });
                cursor_x = cursor_x.saturating_add(repeat);
                repeat = 1;
                position += 1;
            }
            _ => {
                position += 1;
            }
        }
    }
}

/// The columns `run` paints inside the raster, or `None` when it starts at or past the right edge.
fn run_columns(run: &SixelRun) -> Option<std::ops::Range<usize>> {
    (run.column < MAX_SIDE).then(|| run.column..run.column.saturating_add(run.repeat).min(MAX_SIDE))
}

/// The rows of `run`'s band that a set bit paints inside the raster.
fn run_rows(run: &SixelRun) -> impl Iterator<Item = usize> + '_ {
    (0..6usize)
        .filter(|bit| run.bits & (1 << bit) != 0)
        .map(|bit| run.band_top.saturating_add(bit))
        .filter(|row| *row < MAX_SIDE)
}

/// Decode a Sixel payload to `(width, height, BGRA)`, or `None` when it paints no pixel. The image
/// is the rectangle from the origin to the furthest painted pixel; unpainted pixels are
/// transparent black.
pub(super) fn decode_sixel(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let (mut width, mut height) = (0usize, 0usize);
    walk_sixel(data, |run| {
        // The extent grows only where a pixel is painted: a run with no set bit, or one wholly
        // outside the raster, leaves it unchanged.
        if let (Some(columns), Some(lowest)) = (run_columns(run), run_rows(run).last()) {
            width = width.max(columns.end);
            height = height.max(lowest + 1);
        }
    });
    if width == 0 || height == 0 {
        // When: width or height stayed at zero no sixel byte painted anything, so there is no
        // image to hand back.
        return None;
    }
    let mut pixels = vec![0u8; width * height * 4];
    walk_sixel(data, |run| {
        let Some(columns) = run_columns(run) else {
            // When: run_columns is None the run starts past the right edge and paints nothing.
            return;
        };
        let [red, green, blue, alpha] = run.color;
        for row in run_rows(run) {
            for column in columns.clone() {
                let offset = (row * width + column) * 4;
                pixels[offset..offset + 4].copy_from_slice(&[blue, green, red, alpha]);
            }
        }
    });
    Some((width as u32, height as u32, pixels))
}

fn skip_sixel_params(data: &[u8], position: &mut usize) {
    while *position < data.len() {
        match data[*position] {
            b'0'..=b'9' | b';' => *position += 1,
            _ => {
                // When: data holds a byte outside digits and ';' the parameter
                // list has ended, so scanning stops with position on that byte.
                break;
            }
        }
    }
}

fn parse_sixel_number(data: &[u8], position: &mut usize) -> Option<u32> {
    let start = *position;
    let mut value = 0u32;
    while *position < data.len() {
        match data[*position] {
            b'0'..=b'9' => {
                value = value.saturating_mul(10).saturating_add(u32::from(data[*position] - b'0'));
                *position += 1;
            }
            _ => {
                // When: data holds a byte outside the digit range the number is
                // complete, so position stops there and the digits so far form value.
                break;
            }
        }
    }
    (*position > start).then_some(value)
}

fn percent_to_u8(percent: u32) -> u8 {
    ((percent.min(100) * 255 + 50) / 100) as u8
}

#[cfg(test)]
#[path = "media_sixel_tests.rs"]
mod media_sixel_tests;
