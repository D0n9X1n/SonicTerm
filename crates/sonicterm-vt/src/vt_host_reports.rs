//! Reports the terminal sends a program about itself: DECRQM for the stored private modes, the
//! color scheme for DSR 996 and its mode 2031 updates, and the cell size for `CSI 16 t`.

use super::*;
use sonicterm_types::ColorScheme;

/// The theme values a program can read back (`OSC 4`/`10`/`11`/`12` and DSR 996). A theme update
/// that changes any of them is a palette change mode 2031 reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ThemeFingerprint {
    foreground: Option<(u8, u8, u8)>,
    background: Option<(u8, u8, u8)>,
    cursor: Option<(u8, u8, u8)>,
    palette: [Option<(u8, u8, u8)>; 16],
    scheme: Option<ColorScheme>,
}

impl Parser {
    /// The cursor shape and steadiness the program asked for with DECSCUSR; `None` when it asked
    /// for none, or restored the configured cursor.
    #[must_use]
    pub fn cursor_request(&self) -> Option<sonicterm_types::CursorRequest> {
        self.performer.cursor_request
    }

    /// Tell the parser whether the theme is dark or light, which DSR 996 (`CSI ? 996 n`) and mode
    /// 2031 updates report. Call [`Parser::finish_theme_update`] after seeding every theme color.
    pub fn set_theme_color_scheme(&mut self, scheme: ColorScheme) {
        self.performer.theme_color_scheme = Some(scheme);
    }

    /// End one theme update: the first records the pane's starting theme and reports nothing; a
    /// later one that changed a readable color or the scheme returns the `CSI ? 997 ; Ps n` report
    /// to send when the program set mode 2031. The host writes it to the pane outside the parser
    /// lock, since an update runs outside [`Parser::advance`] and has no reply path of its own.
    #[must_use]
    pub fn finish_theme_update(&mut self) -> Option<Vec<u8>> {
        let performer = &mut self.performer;
        let current = ThemeFingerprint {
            foreground: performer.theme_fg,
            background: performer.theme_bg,
            cursor: performer.theme_cursor,
            palette: performer.theme_palette,
            scheme: performer.theme_color_scheme,
        };
        let previous = performer.reported_theme.replace(current);
        let changed = previous.is_some_and(|previous| previous != current);
        if !changed || !performer.color_scheme_updates {
            // When: this is the creation seed, nothing readable `changed`, or the program never set
            // mode 2031, there is nothing to report.
            return None;
        }
        current.scheme.map(|scheme| scheme.report().to_vec())
    }
}

impl Performer {
    /// The DECRQM state of private mode `mode`: 1 set, 2 reset, 0 for a mode this parser does not
    /// both honour and store, so a prober falls back at once instead of waiting for a reply.
    pub(super) fn private_mode_state(&self, mode: u16) -> u8 {
        let stored = match mode {
            1 => Some(self.app_cursor_keys),
            7 => Some(self.grid.autowrap()),
            47 | 1047 | 1049 => Some(self.grid.is_alt()),
            67 => Some(self.backarrow_key),
            1000 => Some(self.mouse_tracking == MouseTracking::Button),
            1002 => Some(self.mouse_tracking == MouseTracking::ButtonMotion),
            1003 => Some(self.mouse_tracking == MouseTracking::AnyMotion),
            1004 => Some(self.focus_reporting),
            1006 => Some(self.mouse_sgr),
            2004 => Some(self.bracketed_paste),
            2026 => Some(self.synchronized_output),
            2031 => Some(self.color_scheme_updates),
            9001 => Some(self.win32_input),
            _ => None,
        };
        match stored {
            Some(true) => 1,
            Some(false) => 2,
            None => 0,
        }
    }

    /// Apply DECSCUSR `value`: store the request and report it to the host; a value past 6 changes
    /// nothing.
    pub(super) fn set_cursor_request(&mut self, value: u16) {
        if let Some(request) = sonicterm_types::CursorRequest::from_decscusr(value) {
            self.cursor_request = request;
            self.events.push(VtEvent::CursorStyle(request));
        }
    }

    /// Answer DSR 996 with the seeded scheme; silent before a theme is seeded, so no scheme is
    /// claimed that the host never stated.
    pub(super) fn report_color_scheme(&self) {
        if let Some(scheme) = self.theme_color_scheme {
            self.reply(scheme.report());
        }
    }

    /// Answer `CSI 16 t` with the cell size in whole pixels, height first, each at least 1;
    /// silent until the host reports a cell size.
    pub(super) fn report_cell_size(&self) {
        if let Some((width_px, height_px)) = self.report_cell_px {
            let whole = |size_px: f32| (size_px.round() as u32).max(1);
            self.reply(format!("\x1b[6;{};{}t", whole(height_px), whole(width_px)).as_bytes());
        }
    }
}
