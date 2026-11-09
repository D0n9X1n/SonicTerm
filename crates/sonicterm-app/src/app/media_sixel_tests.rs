//! The extent-sized Sixel decoder produces exactly the image the full-raster decoder does: same
//! width, height and pixels for palette changes, repeats past the raster, band moves and bits past
//! either edge.

use super::*;

/// The full-raster decoder, the oracle: paints into a `MAX_SIDE` square, then packs the painted
/// rectangle.
fn full_raster_decode(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut palette = [[0u8, 0, 0, 255]; 256];
    palette[1] = [255, 255, 255, 255];
    let mut color_idx = 1usize;
    let mut pixels = vec![0u8; MAX_SIDE * MAX_SIDE * 4];
    let (mut cursor_x, mut cursor_y, mut max_x, mut max_y) = (0usize, 0usize, 0usize, 0usize);
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
                    let mut channels = [0u32; 3];
                    for channel in &mut channels {
                        if data.get(position) == Some(&b';') {
                            position += 1;
                        }
                        *channel = parse_sixel_number(data, &mut position).unwrap_or(0);
                    }
                    if mode == 2 {
                        let [red, green, blue] = channels.map(percent_to_u8);
                        palette[idx] = [red, green, blue, 255];
                    }
                }
            }
            b'!' => {
                position += 1;
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
                let bits = byte - 63;
                for pixel_x in (cursor_x..cursor_x + repeat).filter(|column| *column < MAX_SIDE) {
                    for bit in (0..6).filter(|bit| bits & (1 << bit) != 0) {
                        let pixel_y = cursor_y + bit;
                        if pixel_y >= MAX_SIDE {
                            continue;
                        }
                        let offset = (pixel_y * MAX_SIDE + pixel_x) * 4;
                        let [red, green, blue, alpha] = palette[color_idx];
                        pixels[offset..offset + 4].copy_from_slice(&[blue, green, red, alpha]);
                        max_x = max_x.max(pixel_x + 1);
                        max_y = max_y.max(pixel_y + 1);
                    }
                }
                cursor_x = cursor_x.saturating_add(repeat);
                repeat = 1;
                position += 1;
            }
            _ => position += 1,
        }
    }
    if max_x == 0 || max_y == 0 {
        return None;
    }
    let mut packed = Vec::with_capacity(max_x * max_y * 4);
    for row in 0..max_y {
        let offset = row * MAX_SIDE * 4;
        packed.extend_from_slice(&pixels[offset..offset + max_x * 4]);
    }
    Some((max_x as u32, max_y as u32, packed))
}

/// A deterministic generator, so a failing payload replays exactly.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }
}

/// A random payload mixing every token the decoder reads: raster attributes, palette selects and
/// HLS/RGB definitions, repeats (some past the raster width), band moves, data bytes and noise.
fn random_payload(rng: &mut Lcg) -> Vec<u8> {
    let mut payload = Vec::new();
    for _ in 0..rng.below(120) {
        let token = match rng.below(9) {
            0 => format!("\"1;1;{};{}", rng.below(900), rng.below(900)),
            1 => format!("#{}", rng.below(300)),
            2 => format!(
                "#{};{};{};{};{}",
                rng.below(8),
                rng.below(3),
                rng.below(120),
                rng.below(120),
                rng.below(120)
            ),
            3 => format!("!{}", [1, 7, 300, 1023, 1024, 5000][rng.below(6) as usize]),
            4 => "$".to_owned(),
            // Sometimes many band moves at once, so bands reach and pass the raster's bottom.
            5 => "-".repeat(if rng.below(4) == 0 { 170 } else { 1 }),
            6 => "x".to_owned(),
            _ => char::from(b'?' + rng.below(64) as u8).to_string(),
        };
        payload.extend_from_slice(token.as_bytes());
    }
    payload
}

/// Over seeded random payloads, the extent-sized decoder matches the full-raster oracle exactly.
#[test]
fn the_extent_sized_decode_matches_the_full_raster_decode() {
    for seed in 0..96u64 {
        let payload = random_payload(&mut Lcg(seed));
        assert_eq!(
            decode_sixel(&payload),
            full_raster_decode(&payload),
            "seed {seed}: {}",
            String::from_utf8_lossy(&payload)
        );
    }
}

/// The edges the random payloads may miss: bits past the bottom row, a run that starts past the
/// right edge, a cursor that has saturated, a payload with no set bit and an empty one.
#[test]
fn edge_payloads_match_the_full_raster_decode() {
    let past_bottom = format!("{}~", "-".repeat(170));
    let past_right = "!1024~!5~$#2;2;100;0;0!3~".to_owned();
    let saturated = format!("{}~", "!1024?".repeat(5000));
    for payload in [past_bottom, past_right, saturated, "???-?$?".to_owned(), String::new()] {
        assert_eq!(decode_sixel(payload.as_bytes()), full_raster_decode(payload.as_bytes()));
    }
}

/// A small image allocates only its own pixels, once: no raster at the side cap and no packing
/// copy. A payload that paints nothing allocates nothing.
#[test]
fn a_small_image_allocates_only_its_own_pixels() {
    let counter = crate::lib_tests::CountAllocations::start();
    let decoded = decode_sixel(b"#1!3~");
    let allocations = counter.count();
    drop(counter);
    let (width, height, pixels) = decoded.expect("three full columns decode");
    assert_eq!((width, height), (3, 6));
    assert_eq!(allocations, 1, "one allocation, the image's own pixels");
    assert_eq!(pixels.capacity(), 3 * 6 * 4);

    let counter = crate::lib_tests::CountAllocations::start();
    let nothing = decode_sixel(b"#1???$-?");
    assert_eq!(counter.count(), 0, "a payload with no set bit allocates nothing");
    drop(counter);
    assert!(nothing.is_none());
}
