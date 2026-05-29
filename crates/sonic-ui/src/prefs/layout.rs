//! Layout for the preferences window — redesign per issue #112 Round 2.
//!
//! Window is divided into:
//! - A 188-px-wide left **sidebar** listing categories (nav rail).
//! - A right-hand **content area** padded with 28t/32h, containing one or
//!   more **cards** (radius 14, surface background, 1px subtle border).
//! - A sticky 64-px **footer** with Apply / Cancel buttons.
//!
//! All numbers are in *logical* pixels — the renderer multiplies by the
//! window's scale factor when emitting quads.

use super::controls::Rect;
use super::{PREFS_WIN_H, PREFS_WIN_W};

/// Category list shown in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Font,
    Theme,
    Keymap,
    Window,
    Cursor,
    Advanced,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Font => "Font",
            Category::Theme => "Theme",
            Category::Keymap => "Keymap",
            Category::Window => "Window",
            Category::Cursor => "Cursor",
            Category::Advanced => "Advanced",
        }
    }

    /// One-line subtitle shown beneath the page title.
    pub fn description(self) -> &'static str {
        match self {
            Category::Font => "Choose the typeface and metrics used for terminal text.",
            Category::Theme => "Pick the color theme and preview terminal chrome.",
            Category::Keymap => "Select the keyboard shortcut preset.",
            Category::Window => "Tune window chrome, opacity, blur, and padding.",
            Category::Cursor => "Adjust cursor shape and blink behavior.",
            Category::Advanced => "Set shell startup, scrollback, language, and diagnostics.",
        }
    }

    /// Embedded SVG icon displayed in the sidebar next to the label.
    pub fn icon(self) -> &'static crate::icon::Icon {
        crate::icon::for_category(self)
    }
}

/// All categories in display order.
pub const CATEGORIES: &[Category] = &[
    Category::Font,
    Category::Theme,
    Category::Keymap,
    Category::Window,
    Category::Cursor,
    Category::Advanced,
];

// --- Tunables -----------------------------------------------------------
// All values come straight from the issue #112 Round 2 spec.

/// Sidebar (left nav rail).
pub const SIDEBAR_W: f32 = 188.0;
pub const SIDEBAR_PAD_H: f32 = 16.0;
pub const SIDEBAR_PAD_T: f32 = 20.0;
pub const SIDEBAR_ROW_H: f32 = 40.0;
pub const SIDEBAR_ROW_GAP: f32 = 4.0;
pub const SIDEBAR_ROW_RADIUS: f32 = 10.0;
pub const SIDEBAR_ICON_SLOT: f32 = 16.0;
pub const SIDEBAR_ICON_X: f32 = 12.0;
pub const SIDEBAR_LABEL_X: f32 = 36.0;
pub const SIDEBAR_ACCENT_W: f32 = 3.0;
pub const SIDEBAR_ACCENT_RADIUS: f32 = 1.5;

/// Content area / cards.
pub const CONTENT_PAD_TOP: f32 = 28.0;
pub const CONTENT_PAD_H: f32 = 32.0;
pub const CARD_RADIUS: f32 = 14.0;
pub const CARD_PAD_V: f32 = 18.0;
pub const CARD_PAD_H: f32 = 20.0;
pub const CARD_GAP: f32 = 16.0;

/// Form rows.
pub const ROW_H: f32 = 46.0;
pub const LABEL_W: f32 = 160.0;

/// Controls. The redesigned (issue #173 slice-2) primitives use a 32 px
/// height with a 10 px corner radius for both buttons and comboboxes.
pub const CONTROL_H: f32 = 32.0;
pub const CONTROL_RADIUS: f32 = 10.0;
pub const TOGGLE_W: f32 = 44.0;
pub const TOGGLE_H: f32 = 24.0;
pub const TOGGLE_KNOB: f32 = 20.0;
pub const TOGGLE_KNOB_MARGIN: f32 = 2.0;
pub const SLIDER_TRACK_H: f32 = 4.0;
pub const SLIDER_THUMB: f32 = 16.0;
pub const SWATCH_SIZE: f32 = 22.0;
pub const SWATCH_GAP: f32 = 6.0;
pub const SWATCH_RADIUS: f32 = 6.0;

/// Focus halo painted around any control with `interaction.focused == true`.
/// Halo extends `FOCUS_RING_HALO` px outside the control rect and is drawn
/// with `theme.accent` at reduced alpha. See issue #173 slice-2 visual spec.
pub const FOCUS_RING_HALO: f32 = 4.0;
pub const FOCUS_RING_THICKNESS: f32 = 2.0;

/// Footer.
pub const FOOTER_H: f32 = 64.0;
pub const RESET_LINK_W: f32 = 128.0;
pub const RESET_LINK_H: f32 = 24.0;
pub const BUTTON_H: f32 = 32.0;
/// Pill button corner radius — matches `CONTROL_RADIUS` so buttons and
/// comboboxes appear from the same family (fixes issue #169).
pub const BUTTON_RADIUS: f32 = 10.0;
pub const PRIMARY_BUTTON_W: f32 = 112.0;
pub const SECONDARY_BUTTON_W: f32 = 96.0;
pub const BUTTON_GAP: f32 = 12.0;

/// Preview card (Appearance).
pub const PREVIEW_CARD_H: f32 = 156.0;
pub const PREVIEW_PAD: f32 = 16.0;

/// Title block.
pub const TITLE_X: f32 = 28.0;
pub const TITLE_Y: f32 = 24.0;
pub const TITLE_SIZE: f32 = 20.0;
pub const TITLE_LINE: f32 = 28.0;
pub const SUBTITLE_SIZE: f32 = 12.0;
pub const SUBTITLE_LINE: f32 = 18.0;
pub const SUBTITLE_GAP: f32 = 4.0;

/// Section title / help sizes inside cards.
pub const SECTION_TITLE_SIZE: f32 = 13.0;
pub const SECTION_HELP_SIZE: f32 = 12.0;
pub const SECTION_HELP_MAX_W: f32 = 420.0;

/// Pre-computed rectangles for the window's static chrome. Per-control
/// rects are computed by [`PrefsLayout::form_row`] /
/// [`PrefsLayout::control_slot`].
#[derive(Debug, Clone, Copy)]
pub struct PrefsLayout {
    pub width: f32,
    pub height: f32,
    pub sidebar: Rect,
    /// Vertical divider drawn on the sidebar's right edge.
    pub sidebar_divider: Rect,
    /// Outer content rect (right of sidebar, above footer).
    pub content: Rect,
    /// Title block rect (anchor for "Preferences" + subtitle).
    pub title_block: Rect,
    /// The "card" the form is drawn inside — first card, just below
    /// the title block.
    pub form_card: Rect,
    pub footer: Rect,
    /// Top border line of the footer (1px high).
    pub footer_divider: Rect,
    pub apply_button: Rect,
    pub cancel_button: Rect,
    pub reset_link: Rect,
}

impl PrefsLayout {
    /// Build a layout for the given window size (clamped to the min
    /// from the spec: 680 × 520).
    pub fn new(width: f32, height: f32) -> Self {
        let width = width.max(680.0);
        let height = height.max(520.0);
        let sidebar = Rect::new(0.0, 0.0, SIDEBAR_W, height);
        let sidebar_divider = Rect::new(SIDEBAR_W - 1.0, 0.0, 1.0, height);
        let content_x = SIDEBAR_W;
        let content_w = width - SIDEBAR_W;
        let content_h = height - FOOTER_H;
        let content = Rect::new(content_x, 0.0, content_w, content_h);

        // Title block — sized by typography ramps.
        let title_h = TITLE_LINE + SUBTITLE_GAP + SUBTITLE_LINE;
        let title_block =
            Rect::new(content.x + TITLE_X, TITLE_Y, content.w - TITLE_X - CONTENT_PAD_H, title_h);

        // First card sits below the title with CONTENT_PAD_TOP gap and
        // extends to the bottom of the content area (minus a small
        // breath).
        let card_top = title_block.y + title_block.h + CONTENT_PAD_TOP - 8.0;
        let card_x = content.x + CONTENT_PAD_H;
        let card_w = content.w - CONTENT_PAD_H * 2.0;
        let card_h = (content_h - card_top - CONTENT_PAD_TOP * 0.5).max(120.0);
        let form_card = Rect::new(card_x, card_top, card_w, card_h);

        let footer = Rect::new(0.0, height - FOOTER_H, width, FOOTER_H);
        let footer_divider = Rect::new(0.0, footer.y, width, 1.0);
        let buttons_y = footer.y + (FOOTER_H - BUTTON_H) / 2.0;
        let apply_x = footer.x + footer.w - PRIMARY_BUTTON_W - CONTENT_PAD_H;
        let cancel_x = apply_x - SECONDARY_BUTTON_W - BUTTON_GAP;
        let apply_button = Rect::new(apply_x, buttons_y, PRIMARY_BUTTON_W, BUTTON_H);
        let cancel_button = Rect::new(cancel_x, buttons_y, SECONDARY_BUTTON_W, BUTTON_H);
        let reset_link = Rect::new(
            footer.x + CONTENT_PAD_H,
            footer.y + (FOOTER_H - RESET_LINK_H) / 2.0,
            RESET_LINK_W,
            RESET_LINK_H,
        );
        Self {
            width,
            height,
            sidebar,
            sidebar_divider,
            content,
            title_block,
            form_card,
            footer,
            footer_divider,
            apply_button,
            cancel_button,
            reset_link,
        }
    }

    /// Default-size layout (matches the values in [`PREFS_WIN_W`] / `_H`).
    pub fn default_size() -> Self {
        Self::new(PREFS_WIN_W, PREFS_WIN_H)
    }

    /// Row rect for a category in the sidebar (0-based).
    pub fn category_row(&self, index: usize) -> Rect {
        Rect::new(
            self.sidebar.x + SIDEBAR_PAD_H,
            self.sidebar.y + SIDEBAR_PAD_T + index as f32 * (SIDEBAR_ROW_H + SIDEBAR_ROW_GAP),
            self.sidebar.w - SIDEBAR_PAD_H * 2.0,
            SIDEBAR_ROW_H,
        )
    }

    /// Left-accent bar rect for an active category row.
    pub fn category_accent(&self, index: usize) -> Rect {
        let row = self.category_row(index);
        let bar_h = row.h - 12.0;
        Rect::new(
            row.x - SIDEBAR_PAD_H + 4.0,
            row.y + (row.h - bar_h) / 2.0,
            SIDEBAR_ACCENT_W,
            bar_h,
        )
    }

    /// Hit-test the sidebar: returns the category clicked, if any.
    pub fn hit_category(&self, x: f32, y: f32) -> Option<Category> {
        for (i, c) in CATEGORIES.iter().enumerate() {
            if self.category_row(i).contains(x, y) {
                return Some(*c);
            }
        }
        None
    }

    /// Vertical offset applied to every form row so they sit below the
    /// section title + help block drawn inside the form card. This was
    /// previously a private constant in the renderer, which caused the
    /// rendered controls and the hit-test rects to drift apart — render
    /// applied the offset, hit-test did not. Folding it into the layout
    /// here is the single source of truth.
    pub const ROW_Y_OFFSET: f32 = TITLE_LINE + SUBTITLE_LINE + 16.0;

    /// Row rect for the `n`th control inside the form card (0-based).
    /// Rows are stacked vertically inside the card with `CARD_PAD_V`
    /// top padding and `CARD_PAD_H` horizontal padding. The y position
    /// also includes [`PrefsLayout::ROW_Y_OFFSET`] so callers do not
    /// need to add it themselves (render + hit-test must agree).
    pub fn form_row(&self, n: usize) -> Rect {
        Rect::new(
            self.form_card.x + CARD_PAD_H,
            self.form_card.y + CARD_PAD_V + Self::ROW_Y_OFFSET + n as f32 * ROW_H,
            self.form_card.w - CARD_PAD_H * 2.0,
            ROW_H,
        )
    }

    /// The portion of a form row right of the label, where the actual
    /// control widget is rendered.
    pub fn control_slot(&self, n: usize) -> Rect {
        let row = self.form_row(n);
        // Center a `CONTROL_H` tall slot inside the row.
        let cy = row.y + (row.h - CONTROL_H) / 2.0;
        Rect::new(row.x + LABEL_W, cy, row.w - LABEL_W, CONTROL_H)
    }

    /// Label rect for a form row (left-aligned, vertically centered).
    pub fn label_slot(&self, n: usize) -> Rect {
        let row = self.form_row(n);
        Rect::new(row.x, row.y, LABEL_W, row.h)
    }
}
