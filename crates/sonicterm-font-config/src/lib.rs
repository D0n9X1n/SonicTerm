use ordered_float::NotNan;
use std::fmt::{Display, Formatter};
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

type ErrorCallback = fn(&str);

static CONFIG_GENERATION: AtomicUsize = AtomicUsize::new(1);

lazy_static::lazy_static! {
    static ref CONFIG: Mutex<ConfigHandle> = Mutex::new(ConfigHandle::new(Config::default_config()));
    static ref SHOW_ERROR: Mutex<Option<ErrorCallback>> =
        Mutex::new(Some(|error| log::error!("{}", error)));
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RgbaColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl Default for RgbaColor {
    fn default() -> Self {
        Self { red: 255, green: 255, blue: 255, alpha: 255 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FontStyle {
    Normal,
    Italic,
    Oblique,
}

impl Default for FontStyle {
    fn default() -> Self {
        Self::Normal
    }
}

impl Display for FontStyle {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Normal => write!(f, "Normal"),
            Self::Italic => write!(f, "Italic"),
            Self::Oblique => write!(f, "Oblique"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FontStretch {
    UltraCondensed,
    ExtraCondensed,
    Condensed,
    SemiCondensed,
    Normal,
    SemiExpanded,
    Expanded,
    ExtraExpanded,
    UltraExpanded,
}

impl FontStretch {
    pub fn from_opentype_stretch(width: u16) -> Self {
        match width {
            1 => Self::UltraCondensed,
            2 => Self::ExtraCondensed,
            3 => Self::Condensed,
            4 => Self::SemiCondensed,
            5 => Self::Normal,
            6 => Self::SemiExpanded,
            7 => Self::Expanded,
            8 => Self::ExtraExpanded,
            9 => Self::UltraExpanded,
            _ if width < 1 => Self::UltraCondensed,
            _ => Self::UltraExpanded,
        }
    }

    pub fn to_opentype_stretch(self) -> u16 {
        match self {
            Self::UltraCondensed => 1,
            Self::ExtraCondensed => 2,
            Self::Condensed => 3,
            Self::SemiCondensed => 4,
            Self::Normal => 5,
            Self::SemiExpanded => 6,
            Self::Expanded => 7,
            Self::ExtraExpanded => 8,
            Self::UltraExpanded => 9,
        }
    }
}

impl Default for FontStretch {
    fn default() -> Self {
        Self::Normal
    }
}

impl Display for FontStretch {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UltraCondensed => write!(f, "UltraCondensed"),
            Self::ExtraCondensed => write!(f, "ExtraCondensed"),
            Self::Condensed => write!(f, "Condensed"),
            Self::SemiCondensed => write!(f, "SemiCondensed"),
            Self::Normal => write!(f, "Normal"),
            Self::SemiExpanded => write!(f, "SemiExpanded"),
            Self::Expanded => write!(f, "Expanded"),
            Self::ExtraExpanded => write!(f, "ExtraExpanded"),
            Self::UltraExpanded => write!(f, "UltraExpanded"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FontWeight(u16);

impl FontWeight {
    pub const THIN: FontWeight = FontWeight(100);
    pub const EXTRALIGHT: FontWeight = FontWeight(200);
    pub const LIGHT: FontWeight = FontWeight(300);
    pub const DEMILIGHT: FontWeight = FontWeight(350);
    pub const BOOK: FontWeight = FontWeight(380);
    pub const REGULAR: FontWeight = FontWeight(400);
    pub const MEDIUM: FontWeight = FontWeight(500);
    pub const DEMIBOLD: FontWeight = FontWeight(600);
    pub const BOLD: FontWeight = FontWeight(700);
    pub const EXTRABOLD: FontWeight = FontWeight(800);
    pub const BLACK: FontWeight = FontWeight(900);
    pub const EXTRABLACK: FontWeight = FontWeight(1000);

    pub const fn from_opentype_weight(weight: u16) -> Self {
        Self(weight)
    }

    pub fn to_opentype_weight(self) -> u16 {
        self.0
    }

    pub fn lighter(self) -> Self {
        Self::from_opentype_weight(self.to_opentype_weight().saturating_sub(200))
    }

    pub fn bolder(self) -> Self {
        Self::from_opentype_weight(self.to_opentype_weight() + 200)
    }
}

impl Default for FontWeight {
    fn default() -> Self {
        Self::REGULAR
    }
}

impl Display for FontWeight {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let label = if *self == Self::EXTRABLACK {
            "ExtraBlack"
        } else if *self == Self::BLACK {
            "Black"
        } else if *self == Self::EXTRABOLD {
            "ExtraBold"
        } else if *self == Self::BOLD {
            "Bold"
        } else if *self == Self::DEMIBOLD {
            "DemiBold"
        } else if *self == Self::MEDIUM {
            "Medium"
        } else if *self == Self::REGULAR {
            "Regular"
        } else if *self == Self::BOOK {
            "Book"
        } else if *self == Self::DEMILIGHT {
            "DemiLight"
        } else if *self == Self::LIGHT {
            "Light"
        } else if *self == Self::EXTRALIGHT {
            "ExtraLight"
        } else if *self == Self::THIN {
            "Thin"
        } else {
            return write!(f, "{}", self.0);
        };
        write!(f, "\"{}\"", label)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayPixelGeometry {
    #[default]
    RGB,
    BGR,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FreeTypeLoadTarget {
    #[default]
    Normal,
    Light,
    Mono,
    HorizontalLcd,
    VerticalLcd,
}

bitflags::bitflags! {
    #[derive(Default)]
    pub struct FreeTypeLoadFlags: u32 {
        const DEFAULT = 0;
        const NO_HINTING = 2;
        const NO_BITMAP = 8;
        const FORCE_AUTOHINT = 32;
        const MONOCHROME = 4096;
        const NO_AUTOHINT = 32768;
        const NO_SVG = 16777216;
        const SVG_ONLY = 8388608;
    }
}

impl FreeTypeLoadFlags {
    pub fn default_hidpi() -> Self {
        Self::NO_HINTING
    }
}

impl Display for FreeTypeLoadFlags {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if *self == Self::DEFAULT {
            parts.push("DEFAULT");
        }
        if self.contains(Self::NO_HINTING) {
            parts.push("NO_HINTING");
        }
        if self.contains(Self::NO_BITMAP) {
            parts.push("NO_BITMAP");
        }
        if self.contains(Self::FORCE_AUTOHINT) {
            parts.push("FORCE_AUTOHINT");
        }
        if self.contains(Self::MONOCHROME) {
            parts.push("MONOCHROME");
        }
        if self.contains(Self::NO_AUTOHINT) {
            parts.push("NO_AUTOHINT");
        }
        if self.contains(Self::NO_SVG) {
            parts.push("NO_SVG");
        }
        if self.contains(Self::SVG_ONLY) {
            parts.push("SVG_ONLY");
        }
        write!(f, "{}", parts.join("|"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FontAttributes {
    pub family: String,
    pub weight: FontWeight,
    pub stretch: FontStretch,
    pub style: FontStyle,
    pub is_fallback: bool,
    pub is_synthetic: bool,
    pub harfbuzz_features: Option<Vec<String>>,
    pub freetype_load_target: Option<FreeTypeLoadTarget>,
    pub freetype_render_target: Option<FreeTypeLoadTarget>,
    pub freetype_load_flags: Option<FreeTypeLoadFlags>,
    pub scale: Option<NotNan<f64>>,
    pub assume_emoji_presentation: Option<bool>,
}

impl FontAttributes {
    pub fn new(family: &str) -> Self {
        Self { family: family.into(), ..Default::default() }
    }

    pub fn new_fallback(family: &str) -> Self {
        Self { family: family.into(), is_fallback: true, ..Default::default() }
    }
}

impl Default for FontAttributes {
    fn default() -> Self {
        Self {
            family: "JetBrains Mono".into(),
            weight: FontWeight::default(),
            stretch: FontStretch::default(),
            style: FontStyle::default(),
            is_fallback: false,
            is_synthetic: false,
            harfbuzz_features: None,
            freetype_load_target: None,
            freetype_render_target: None,
            freetype_load_flags: None,
            scale: None,
            assume_emoji_presentation: None,
        }
    }
}

impl Display for FontAttributes {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "sonicterm.font('{}', {{weight={}, stretch='{}', style={}}})",
            self.family, self.weight, self.stretch, self.style
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextStyle {
    pub font: Vec<FontAttributes>,
    pub foreground: Option<RgbaColor>,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self { foreground: None, font: vec![FontAttributes::default()] }
    }
}

impl TextStyle {
    pub fn reduce_first_font_to_family(&self) -> Self {
        fn reduce(mut family: &str) -> String {
            loop {
                let start = family;
                for suffix in [
                    "Black",
                    "Bold",
                    "Book",
                    "Condensed",
                    "Demi",
                    "Expanded",
                    "Extra",
                    "Italic",
                    "Light",
                    "Medium",
                    "Regular",
                    "Semi",
                    "Thin",
                    "Ultra",
                ] {
                    family = family.trim().trim_end_matches(suffix);
                }
                if family == start {
                    break;
                }
            }
            family.trim().to_string()
        }

        Self {
            foreground: self.foreground.clone(),
            font: self
                .font
                .iter()
                .enumerate()
                .map(|(idx, attr)| {
                    let mut attr = attr.clone();
                    if idx == 0 {
                        attr.family = reduce(&attr.family);
                    }
                    attr
                })
                .collect(),
        }
    }

    pub fn make_bold(&self) -> Self {
        Self {
            foreground: self.foreground.clone(),
            font: self
                .font
                .iter()
                .map(|attr| {
                    let mut attr = attr.clone();
                    attr.weight = attr.weight.bolder();
                    attr.is_synthetic = true;
                    attr
                })
                .collect(),
        }
    }

    pub fn make_half_bright(&self) -> Self {
        Self {
            foreground: self.foreground.clone(),
            font: self
                .font
                .iter()
                .map(|attr| {
                    let mut attr = attr.clone();
                    attr.weight = attr.weight.lighter();
                    attr.is_synthetic = true;
                    attr
                })
                .collect(),
        }
    }

    pub fn make_italic(&self) -> Self {
        Self {
            foreground: self.foreground.clone(),
            font: self
                .font
                .iter()
                .map(|attr| {
                    let mut attr = attr.clone();
                    attr.style = FontStyle::Italic;
                    attr.is_synthetic = true;
                    attr
                })
                .collect(),
        }
    }

    pub fn font_with_fallback(&self) -> Vec<FontAttributes> {
        let mut font = self.font.clone();
        let mut default_font = FontAttributes::default();

        if !font.iter().any(|attr| *attr == default_font) {
            default_font.is_fallback = true;
            font.push(default_font);
        }

        font.push(FontAttributes::new_fallback("Noto Color Emoji"));
        font.push(FontAttributes::new_fallback("Symbols Nerd Font Mono"));
        font
    }
}

#[derive(Debug, Default, Clone)]
pub struct StyleRule {
    pub font: TextStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowSquareGlyphOverflow {
    Never,
    Always,
    WhenFollowedBySpace,
}

impl Default for AllowSquareGlyphOverflow {
    fn default() -> Self {
        Self::WhenFollowedBySpace
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontLocatorSelection {
    FontConfig,
    Gdi,
    CoreText,
    ConfigDirsOnly,
}

impl Default for FontLocatorSelection {
    fn default() -> Self {
        if cfg!(windows) {
            Self::Gdi
        } else if cfg!(target_os = "macos") {
            Self::CoreText
        } else {
            Self::FontConfig
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub enum FontRasterizerSelection {
    #[default]
    FreeType,
    Harfbuzz,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum FontShaperSelection {
    Allsorts,
    #[default]
    Harfbuzz,
}

#[derive(Debug, Default, Clone)]
pub struct WindowFrameConfig {
    pub font: Option<TextStyle>,
    pub font_size: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub font_size: f64,
    pub line_height: f64,
    pub cell_width: f64,
    pub font_dirs: Vec<PathBuf>,
    pub font: TextStyle,
    pub font_rules: Vec<StyleRule>,
    pub window_frame: WindowFrameConfig,
    pub char_select_font: Option<TextStyle>,
    pub char_select_font_size: f64,
    pub command_palette_font: Option<TextStyle>,
    pub command_palette_font_size: f64,
    pub pane_select_font: Option<TextStyle>,
    pub pane_select_font_size: f64,
    pub font_locator: FontLocatorSelection,
    pub font_rasterizer: FontRasterizerSelection,
    pub font_colr_rasterizer: FontRasterizerSelection,
    pub font_shaper: FontShaperSelection,
    pub harfbuzz_features: Vec<String>,
    pub display_pixel_geometry: DisplayPixelGeometry,
    pub freetype_load_target: FreeTypeLoadTarget,
    pub freetype_render_target: Option<FreeTypeLoadTarget>,
    pub freetype_load_flags: Option<FreeTypeLoadFlags>,
    pub freetype_interpreter_version: Option<u32>,
    pub freetype_pcf_long_family_names: bool,
    pub ignore_svg_fonts: bool,
    pub search_font_dirs_for_fallback: bool,
    pub sort_fallback_fonts_by_coverage: bool,
    pub warn_about_missing_glyphs: bool,
    pub use_cap_height_to_scale_fallback_fonts: bool,
    generation: usize,
}

impl Config {
    pub fn default_config() -> Self {
        Self {
            font_size: 12.0,
            line_height: 1.0,
            cell_width: 1.0,
            font_dirs: Vec::new(),
            font: TextStyle::default(),
            font_rules: Vec::new(),
            window_frame: WindowFrameConfig::default(),
            char_select_font: None,
            char_select_font_size: 16.0,
            command_palette_font: None,
            command_palette_font_size: 12.0,
            pane_select_font: None,
            pane_select_font_size: 24.0,
            font_locator: FontLocatorSelection::default(),
            font_rasterizer: FontRasterizerSelection::default(),
            font_colr_rasterizer: FontRasterizerSelection::default(),
            font_shaper: FontShaperSelection::default(),
            harfbuzz_features: Vec::new(),
            display_pixel_geometry: DisplayPixelGeometry::default(),
            freetype_load_target: FreeTypeLoadTarget::default(),
            freetype_render_target: None,
            freetype_load_flags: None,
            freetype_interpreter_version: None,
            freetype_pcf_long_family_names: true,
            ignore_svg_fonts: false,
            search_font_dirs_for_fallback: true,
            sort_fallback_fonts_by_coverage: true,
            warn_about_missing_glyphs: true,
            use_cap_height_to_scale_fallback_fonts: true,
            generation: CONFIG_GENERATION.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub fn generation(&self) -> usize {
        self.generation
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::default_config()
    }
}

#[derive(Clone, Debug)]
pub struct ConfigHandle {
    inner: Arc<Config>,
}

impl ConfigHandle {
    pub fn new(config: Config) -> Self {
        Self { inner: Arc::new(config) }
    }

    pub fn generation(&self) -> usize {
        self.inner.generation()
    }
}

impl Deref for ConfigHandle {
    type Target = Config;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub fn configuration() -> ConfigHandle {
    CONFIG.lock().expect("font config lock poisoned").clone()
}

pub fn use_this_configuration(config: Config) {
    *CONFIG.lock().expect("font config lock poisoned") = ConfigHandle::new(config);
}

pub fn show_error(error: &str) {
    if let Some(callback) = *SHOW_ERROR.lock().expect("show error lock poisoned") {
        callback(error);
    }
}
