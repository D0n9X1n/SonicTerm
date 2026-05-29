use crate::prefs::Category;

/// Embedded SVG icon metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icon {
    pub key: &'static str,
    pub svg: &'static str,
}

pub const FONT: Icon = Icon { key: "font", svg: include_str!("../../../assets/icons/ui/font.svg") };
pub const THEME: Icon =
    Icon { key: "theme", svg: include_str!("../../../assets/icons/ui/theme.svg") };
pub const KEYMAP: Icon =
    Icon { key: "keymap", svg: include_str!("../../../assets/icons/ui/keymap.svg") };
pub const WINDOW: Icon =
    Icon { key: "window", svg: include_str!("../../../assets/icons/ui/window.svg") };
pub const CURSOR: Icon =
    Icon { key: "cursor", svg: include_str!("../../../assets/icons/ui/cursor.svg") };
pub const ADVANCED: Icon =
    Icon { key: "advanced", svg: include_str!("../../../assets/icons/ui/advanced.svg") };

pub const ALL: &[Icon] = &[FONT, THEME, KEYMAP, WINDOW, CURSOR, ADVANCED];

pub fn for_category(cat: Category) -> &'static Icon {
    match cat {
        Category::Font => &FONT,
        Category::Theme => &THEME,
        Category::Keymap => &KEYMAP,
        Category::Window => &WINDOW,
        Category::Cursor => &CURSOR,
        Category::Advanced => &ADVANCED,
    }
}

/// A tiny SVG rasterizer for Sonic's bundled, single-color Lucide line icons.
///
/// It intentionally supports only the subset used by `assets/icons/ui`: paths,
/// lines, polylines, rects, and circles. The output is an alpha mask that the
/// GPU render path tints with theme colors at draw time.
pub fn rasterize_alpha(icon: &Icon, size_px: u32) -> Vec<u8> {
    let mut canvas = AlphaCanvas::new(size_px);
    let viewport = 24.0;
    let scale = size_px as f32 / viewport;
    let stroke_width =
        attr(icon.svg, "stroke-width").and_then(|s| s.parse::<f32>().ok()).unwrap_or(2.0) * scale;

    for tag in elements(icon.svg) {
        if tag.starts_with("line ") {
            if let (Some(x1), Some(y1), Some(x2), Some(y2)) =
                (num(tag, "x1"), num(tag, "y1"), num(tag, "x2"), num(tag, "y2"))
            {
                canvas.stroke_polyline(
                    &[(x1 * scale, y1 * scale), (x2 * scale, y2 * scale)],
                    stroke_width,
                );
            }
        } else if tag.starts_with("polyline ") {
            if let Some(points) = attr(tag, "points") {
                let pts: Vec<(f32, f32)> = points
                    .split_whitespace()
                    .filter_map(|pair| {
                        let (x, y) = pair.split_once(',')?;
                        Some((x.parse::<f32>().ok()? * scale, y.parse::<f32>().ok()? * scale))
                    })
                    .collect();
                canvas.stroke_polyline(&pts, stroke_width);
            }
        } else if tag.starts_with("rect ") {
            if let (Some(x), Some(y), Some(w), Some(h)) =
                (num(tag, "x"), num(tag, "y"), num(tag, "width"), num(tag, "height"))
            {
                canvas.stroke_rect(x * scale, y * scale, w * scale, h * scale, stroke_width);
            }
        } else if tag.starts_with("circle ") {
            if let (Some(cx), Some(cy), Some(r)) = (num(tag, "cx"), num(tag, "cy"), num(tag, "r")) {
                let fill = attr(tag, "fill").is_some_and(|v| v != "none");
                canvas.circle(cx * scale, cy * scale, r * scale, stroke_width, fill);
            }
        } else if tag.starts_with("path ") {
            if let Some(d) = attr(tag, "d") {
                canvas.path(d, scale, stroke_width);
            }
        }
    }

    canvas.pixels
}

fn elements(svg: &str) -> impl Iterator<Item = &str> {
    svg.split('<')
        .filter_map(|s| s.split_once('>').map(|(tag, _)| tag.trim().trim_end_matches('/').trim()))
        .filter(|tag| {
            tag.starts_with("path ")
                || tag.starts_with("line ")
                || tag.starts_with("polyline ")
                || tag.starts_with("rect ")
                || tag.starts_with("circle ")
        })
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn num(tag: &str, name: &str) -> Option<f32> {
    attr(tag, name)?.parse().ok()
}

#[derive(Clone)]
struct AlphaCanvas {
    size: u32,
    pixels: Vec<u8>,
}

impl AlphaCanvas {
    fn new(size: u32) -> Self {
        Self { size, pixels: vec![0; (size * size) as usize] }
    }

    fn stroke_polyline(&mut self, points: &[(f32, f32)], width: f32) {
        for segment in points.windows(2) {
            let [(x1, y1), (x2, y2)] = segment else { continue };
            self.line(*x1, *y1, *x2, *y2, width);
        }
    }

    fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, width: f32) {
        let radius = width / 2.0;
        let min_x = (x1.min(x2) - radius - 1.0).floor() as i32;
        let max_x = (x1.max(x2) + radius + 1.0).ceil() as i32;
        let min_y = (y1.min(y2) - radius - 1.0).floor() as i32;
        let max_y = (y1.max(y2) + radius + 1.0).ceil() as i32;
        let dx = x2 - x1;
        let dy = y2 - y1;
        let len2 = dx * dx + dy * dy;
        for py in min_y..=max_y {
            for px in min_x..=max_x {
                let cx = px as f32 + 0.5;
                let cy = py as f32 + 0.5;
                let t = if len2 <= f32::EPSILON {
                    0.0
                } else {
                    (((cx - x1) * dx + (cy - y1) * dy) / len2).clamp(0.0, 1.0)
                };
                let nx = x1 + t * dx;
                let ny = y1 + t * dy;
                self.cover(px, py, (cx - nx).hypot(cy - ny) - radius);
            }
        }
    }

    fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32, width: f32) {
        self.stroke_polyline(&[(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)], width);
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, width: f32, fill: bool) {
        let outer = if fill { r } else { r + width / 2.0 };
        let min_x = (cx - outer - 1.0).floor() as i32;
        let max_x = (cx + outer + 1.0).ceil() as i32;
        let min_y = (cy - outer - 1.0).floor() as i32;
        let max_y = (cy + outer + 1.0).ceil() as i32;
        for py in min_y..=max_y {
            for px in min_x..=max_x {
                let dist = (px as f32 + 0.5 - cx).hypot(py as f32 + 0.5 - cy);
                let sdf = if fill { dist - r } else { (dist - r).abs() - width / 2.0 };
                self.cover(px, py, sdf);
            }
        }
    }

    fn path(&mut self, d: &str, scale: f32, width: f32) {
        let tokens = path_tokens(d);
        let mut i = 0;
        let mut cmd = 'M';
        let mut current = (0.0, 0.0);
        let mut start = (0.0, 0.0);
        while i < tokens.len() {
            if let Some(c) = tokens[i].chars().next().filter(|c| c.is_ascii_alphabetic()) {
                cmd = c;
                i += 1;
            }
            match cmd {
                'M' | 'm' => {
                    if i + 1 >= tokens.len() {
                        break;
                    }
                    let p = (parse(&tokens[i]), parse(&tokens[i + 1]));
                    let abs = if cmd == 'm' { (current.0 + p.0, current.1 + p.1) } else { p };
                    current = (abs.0 * scale, abs.1 * scale);
                    start = current;
                    i += 2;
                    cmd = if cmd == 'm' { 'l' } else { 'L' };
                }
                'L' | 'l' => {
                    if i + 1 >= tokens.len() {
                        break;
                    }
                    let p = (parse(&tokens[i]), parse(&tokens[i + 1]));
                    let next = if cmd == 'l' {
                        (current.0 + p.0 * scale, current.1 + p.1 * scale)
                    } else {
                        (p.0 * scale, p.1 * scale)
                    };
                    self.line(current.0, current.1, next.0, next.1, width);
                    current = next;
                    i += 2;
                }
                'H' | 'h' => {
                    if i >= tokens.len() {
                        break;
                    }
                    let x = parse(&tokens[i]) * scale;
                    let next = if cmd == 'h' { (current.0 + x, current.1) } else { (x, current.1) };
                    self.line(current.0, current.1, next.0, next.1, width);
                    current = next;
                    i += 1;
                }
                'V' | 'v' => {
                    if i >= tokens.len() {
                        break;
                    }
                    let y = parse(&tokens[i]) * scale;
                    let next = if cmd == 'v' { (current.0, current.1 + y) } else { (current.0, y) };
                    self.line(current.0, current.1, next.0, next.1, width);
                    current = next;
                    i += 1;
                }
                'C' | 'c' => {
                    if i + 5 >= tokens.len() {
                        break;
                    }
                    let raw = [
                        (parse(&tokens[i]), parse(&tokens[i + 1])),
                        (parse(&tokens[i + 2]), parse(&tokens[i + 3])),
                        (parse(&tokens[i + 4]), parse(&tokens[i + 5])),
                    ];
                    let pts = if cmd == 'c' {
                        raw.map(|p| (current.0 + p.0 * scale, current.1 + p.1 * scale))
                    } else {
                        raw.map(|p| (p.0 * scale, p.1 * scale))
                    };
                    self.cubic(current, pts[0], pts[1], pts[2], width);
                    current = pts[2];
                    i += 6;
                }
                'S' | 's' => {
                    if i + 3 >= tokens.len() {
                        break;
                    }
                    let raw = [
                        (parse(&tokens[i]), parse(&tokens[i + 1])),
                        (parse(&tokens[i + 2]), parse(&tokens[i + 3])),
                    ];
                    let pts = if cmd == 's' {
                        raw.map(|p| (current.0 + p.0 * scale, current.1 + p.1 * scale))
                    } else {
                        raw.map(|p| (p.0 * scale, p.1 * scale))
                    };
                    self.cubic(current, current, pts[0], pts[1], width);
                    current = pts[1];
                    i += 4;
                }
                'Z' | 'z' => {
                    self.line(current.0, current.1, start.0, start.1, width);
                    current = start;
                }
                _ => break,
            }
        }
    }

    fn cubic(
        &mut self,
        p0: (f32, f32),
        p1: (f32, f32),
        p2: (f32, f32),
        p3: (f32, f32),
        width: f32,
    ) {
        let mut prev = p0;
        for step in 1..=24 {
            let t = step as f32 / 24.0;
            let mt = 1.0 - t;
            let p = (
                mt.powi(3) * p0.0
                    + 3.0 * mt.powi(2) * t * p1.0
                    + 3.0 * mt * t.powi(2) * p2.0
                    + t.powi(3) * p3.0,
                mt.powi(3) * p0.1
                    + 3.0 * mt.powi(2) * t * p1.1
                    + 3.0 * mt * t.powi(2) * p2.1
                    + t.powi(3) * p3.1,
            );
            self.line(prev.0, prev.1, p.0, p.1, width);
            prev = p;
        }
    }

    fn cover(&mut self, x: i32, y: i32, sdf: f32) {
        if x < 0 || y < 0 || x >= self.size as i32 || y >= self.size as i32 {
            return;
        }
        let alpha = (1.0 - sdf).clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return;
        }
        let idx = (y as u32 * self.size + x as u32) as usize;
        self.pixels[idx] = self.pixels[idx].max((alpha * 255.0).round() as u8);
    }
}

fn path_tokens(d: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev = '\0';
    for ch in d.chars() {
        if ch.is_ascii_alphabetic() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            out.push(ch.to_string());
        } else if ch == ',' || ch.is_whitespace() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else if ch == '-' && prev != 'e' && prev != 'E' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(ch);
        } else {
            cur.push(ch);
        }
        prev = ch;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn parse(s: &str) -> f32 {
    s.parse().unwrap_or(0.0)
}
