//! The About window's content: the facts it shows, their labelled rows, the text Copy puts on the
//! clipboard, and where its Copy button sits. Pure: the window code gathers the facts and labels,
//! this module only lays them out, so every row is tested from fixed inputs.

use std::path::Path;

use unicode_width::UnicodeWidthStr;

use super::memory_report::sanitize_external;

/// The longest config error the Config row shows, in characters; the rest is cut with `…`.
pub(crate) const CONFIG_ERROR_CHARS: usize = 120;

/// How the renderer the About window describes draws its frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RendererKind {
    /// No window has a renderer yet.
    NotStarted,
    /// A hardware GPU adapter.
    Gpu,
    /// The wgpu adapter is a CPU rasterizer.
    SoftwareAdapter,
    /// The software degrade policy is forced on a hardware adapter.
    SoftwareForced,
    /// Frames go to the Windows GDI presenter.
    Gdi,
}

/// The renderer row's facts, read from what the renderer selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RendererFacts {
    /// How frames are drawn.
    pub(crate) kind: RendererKind,
    /// The adapter's name, empty when the platform reports none.
    pub(crate) adapter: String,
    /// The graphics API, such as `Metal` or `DirectX 12`.
    pub(crate) backend: String,
}

/// Whether the config file loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigStatus {
    /// The file loaded.
    Loaded,
    /// There is no file, so the defaults apply.
    Missing,
    /// The last load failed with this message.
    Failed(String),
}

/// Everything the About window reports.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AboutFacts {
    /// The package version.
    pub(crate) version: String,
    /// The short build commit, empty when unknown.
    pub(crate) commit: String,
    /// The build commit's date, `YYYY-MM-DD`, empty when unknown.
    pub(crate) date: String,
    /// The OS name and version.
    pub(crate) os: String,
    /// The CPU architecture, as the OS names it.
    pub(crate) arch: String,
    /// The renderer row.
    pub(crate) renderer: RendererFacts,
    /// The terminal font family.
    pub(crate) font_family: String,
    /// The live font size, in points.
    pub(crate) font_size_pt: f32,
    /// The active theme's name.
    pub(crate) theme: String,
    /// The last theme reload error; the active theme stays in use.
    pub(crate) theme_error: Option<String>,
    /// The active keymap's name.
    pub(crate) keymap: String,
    /// The last keymap reload error; the active keymap stays in use.
    pub(crate) keymap_error: Option<String>,
    /// The config file, shown with `~` under the home directory.
    pub(crate) config_path: String,
    /// Whether the config file loaded.
    pub(crate) config_status: ConfigStatus,
    /// The log folder, shown with `~` under the home directory.
    pub(crate) log_dir: String,
}

/// The localized words the About window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AboutLabels {
    /// The version row's spoken label; the row itself shows no label.
    pub(crate) version: String,
    /// The OS row's spoken label; the row itself shows no label.
    pub(crate) system: String,
    /// The renderer row's label.
    pub(crate) renderer: String,
    /// The font row's label.
    pub(crate) font: String,
    /// The theme row's label.
    pub(crate) theme: String,
    /// The keymap label, beside the theme on one row.
    pub(crate) keymap: String,
    /// The config row's label.
    pub(crate) config: String,
    /// The logs row's label.
    pub(crate) logs: String,
    /// The hardware renderer, `GPU`.
    pub(crate) gpu: String,
    /// A CPU-rasterizer adapter.
    pub(crate) software: String,
    /// Software forced on a hardware adapter.
    pub(crate) software_forced: String,
    /// The Windows GDI presenter.
    pub(crate) gdi: String,
    /// No renderer yet.
    pub(crate) not_started: String,
    /// The config file loaded.
    pub(crate) config_loaded: String,
    /// No config file, defaults in use.
    pub(crate) config_missing: String,
    /// The Copy button's label, such as `[ Copy ]`.
    pub(crate) copy_button: String,
    /// The hint under the button, naming the copy and close keys.
    pub(crate) hint: String,
}

/// The rendered About window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AboutView {
    /// Exactly what the pane shows.
    pub(crate) text: String,
    /// What Copy puts on the clipboard: the report rows, without the button or hint.
    pub(crate) copy_text: String,
    /// The Copy button's row, counted from the first line.
    pub(crate) button_row: u16,
    /// The button's first column and the column after its last.
    pub(crate) button_cols: (u16, u16),
    /// The widest line, in terminal columns.
    pub(crate) width: u16,
    /// The Copy button's label, which a press is matched against on the drawn row.
    pub(crate) button_label: String,
    /// Every report row as `(label, value)`, in order, for the accessibility tree.
    pub(crate) accessible: Vec<(String, String)>,
}

/// Display width of `text` in terminal columns, as a `u16`.
fn columns(text: &str) -> u16 {
    u16::try_from(UnicodeWidthStr::width(text)).unwrap_or(u16::MAX)
}

/// `label` padded with spaces to `width` columns, then the three-space gap before its value.
fn padded(label: &str, width: u16) -> String {
    let fill = usize::from(width.saturating_sub(columns(label)));
    format!("{label}{}   ", " ".repeat(fill))
}

/// Lay `facts` out under `labels` for a pane `pane_cols` wide. The Copy button is right-aligned to
/// the widest row, or to the pane's edge when the pane is narrower, so it never wraps.
pub(crate) fn render(facts: &AboutFacts, labels: &AboutLabels, pane_cols: u16) -> AboutView {
    let clean = |text: &str| sanitize_external(text);
    let label_width = [
        &labels.renderer,
        &labels.font,
        &labels.theme,
        &labels.keymap,
        &labels.config,
        &labels.logs,
    ]
    .into_iter()
    .map(|label| columns(label))
    .max()
    .unwrap_or(0);
    let config_note = match &facts.config_status {
        ConfigStatus::Loaded => format!("✓ {}", labels.config_loaded),
        ConfigStatus::Missing => format!("– {}", labels.config_missing),
        ConfigStatus::Failed(error) => error_note(error),
    };
    let rows = [
        version_line(&facts.version, &facts.commit, &facts.date),
        format!("{} · {}", clean(&facts.os), clean(&facts.arch)),
        format!(
            "{}{}",
            padded(&labels.renderer, label_width),
            renderer_value(&facts.renderer, labels)
        ),
        format!(
            "{}{} {} pt",
            padded(&labels.font, label_width),
            clean(&facts.font_family),
            point_size(facts.font_size_pt)
        ),
    ];
    let mut rows = Vec::from(rows);
    if facts.theme_error.is_none() && facts.keymap_error.is_none() {
        // Both loaded: they share one row, as the window's layout shows them.
        rows.push(format!(
            "{}{}   {}  {}",
            padded(&labels.theme, label_width),
            clean(&facts.theme),
            labels.keymap,
            clean(&facts.keymap)
        ));
    } else {
        // When: theme_error or keymap_error is set, each gets its own row with the active name and error.
        for (label, name, error) in [
            (&labels.theme, &facts.theme, &facts.theme_error),
            (&labels.keymap, &facts.keymap, &facts.keymap_error),
        ] {
            let note = error.as_deref().map(|error| format!("  {}", error_note(error)));
            rows.push(format!(
                "{}{}{}",
                padded(label, label_width),
                clean(name),
                note.unwrap_or_default()
            ));
        }
    }
    rows.extend([
        format!(
            "{}{}  {config_note}",
            padded(&labels.config, label_width),
            clean(&facts.config_path)
        ),
        format!("{}{}", padded(&labels.logs, label_width), clean(&facts.log_dir)),
    ]);
    let mut accessible = vec![
        (labels.version.clone(), version_line(&facts.version, &facts.commit, &facts.date)),
        (labels.system.clone(), format!("{} · {}", clean(&facts.os), clean(&facts.arch))),
        (labels.renderer.clone(), renderer_value(&facts.renderer, labels)),
        (
            labels.font.clone(),
            format!("{} {} pt", clean(&facts.font_family), point_size(facts.font_size_pt)),
        ),
    ];
    for (label, name, error) in [
        (&labels.theme, &facts.theme, &facts.theme_error),
        (&labels.keymap, &facts.keymap, &facts.keymap_error),
    ] {
        let note = error.as_deref().map(|error| format!(" {}", error_note(error)));
        accessible.push((label.clone(), format!("{}{}", clean(name), note.unwrap_or_default())));
    }
    accessible
        .push((labels.config.clone(), format!("{} {config_note}", clean(&facts.config_path))));
    accessible.push((labels.logs.clone(), clean(&facts.log_dir)));
    let mut copy_text = String::new();
    for row in &rows {
        copy_text.push_str(row);
        copy_text.push('\n');
    }
    let width = rows.iter().chain([&labels.hint]).map(|row| columns(row)).max().unwrap_or(0);
    let button_width = columns(&labels.copy_button);
    let button_start = width.min(pane_cols).saturating_sub(button_width);
    let width = width.max(button_width);
    let text = format!(
        "{copy_text}\n{}{}\n{}\n",
        " ".repeat(usize::from(button_start)),
        labels.copy_button,
        labels.hint
    );
    AboutView {
        text,
        copy_text,
        // The rows, then a blank line, then the button.
        button_row: u16::try_from(rows.len() + 1).unwrap_or(u16::MAX),
        button_cols: (button_start, button_start + button_width),
        width,
        button_label: labels.copy_button.clone(),
        accessible,
    }
}

/// A load error as a row note: `✗`, its first line with control characters made inert, cut at
/// [`CONFIG_ERROR_CHARS`] characters with `…`.
fn error_note(error: &str) -> String {
    let first = sanitize_external(error.lines().next().unwrap_or_default());
    let mut message: String = first.chars().take(CONFIG_ERROR_CHARS).collect();
    if first.chars().count() > CONFIG_ERROR_CHARS {
        // A cut message ends in `…`, so the reader knows the rest is in the log.
        message.push('…');
    }
    format!("✗ {message}")
}

/// `size_pt` with at most two decimals and no trailing zeros: `13.5`, `13`.
fn point_size(size_pt: f32) -> String {
    let text = format!("{size_pt:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// The columns of `label` in drawn row `row_text` when that row is exactly the label after its
/// indent: its first column and the column after its last. A row that only mentions it is not it.
pub(crate) fn button_span(row_text: &str, label: &str) -> Option<(u16, u16)> {
    let label = label.trim();
    (!label.is_empty() && row_text.trim() == label).then(|| {
        let indent = columns(&row_text[..row_text.len() - row_text.trim_start().len()]);
        (indent, indent + columns(label))
    })
}

/// The version row: `SonicTerm <version> (<commit>, <date>)`, without the parentheses when both
/// are unknown.
pub(crate) fn version_line(version: &str, commit: &str, date: &str) -> String {
    let build: Vec<&str> = [commit, date].into_iter().filter(|part| !part.is_empty()).collect();
    if build.is_empty() {
        // When: build is empty, neither the commit nor the date is known, so none is shown.
        return format!("SonicTerm {version}");
    }
    format!("SonicTerm {version} ({})", build.join(", "))
}

/// The renderer row's value.
pub(crate) fn renderer_value(renderer: &RendererFacts, labels: &AboutLabels) -> String {
    let path = match renderer.kind {
        RendererKind::NotStarted => {
            // When: kind is NotStarted, no renderer exists, so there is no adapter to name.
            return labels.not_started.clone();
        }
        RendererKind::Gdi => &labels.gdi,
        RendererKind::Gpu => &labels.gpu,
        RendererKind::SoftwareAdapter => &labels.software,
        RendererKind::SoftwareForced => &labels.software_forced,
    };
    let adapter = sanitize_external(&renderer.adapter);
    let backend = sanitize_external(&renderer.backend);
    match (adapter.trim().is_empty(), backend.is_empty()) {
        (false, false) => format!("{path} · {adapter} ({backend})"),
        (false, true) => format!("{path} · {adapter}"),
        (true, false) => format!("{path} ({backend})"),
        (true, true) => path.clone(),
    }
}

/// `path` with `~` for `home` when it is inside it.
pub(crate) fn home_relative(path: &Path, home: Option<&Path>) -> String {
    // `strip_prefix` compares whole components, so `/Users/meow` is not inside `/Users/me`.
    let relative = home
        .filter(|home| !home.as_os_str().is_empty())
        .and_then(|home| path.strip_prefix(home).ok());
    match relative {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => path.display().to_string(),
    }
}

/// The architecture as `os` names it: `arm64` on macOS, `x64` and `ARM64` on Windows.
pub(crate) fn arch_label(os: &str, arch: &str) -> String {
    match (os, arch) {
        ("macos", "aarch64") => "arm64",
        ("windows", "x86_64") => "x64",
        ("windows", "aarch64") => "ARM64",
        _ => arch,
    }
    .to_owned()
}

/// The Linux distribution from `/etc/os-release`: `PRETTY_NAME`, or `NAME VERSION_ID`.
// Only the Linux build reads os-release; the other builds test this parser.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn linux_label(os_release: &str) -> Option<String> {
    let value = |key: &str| {
        os_release.lines().find_map(|line| {
            let rest = line.strip_prefix(key)?.strip_prefix('=')?;
            let text = rest.trim().trim_matches('"').trim_matches('\'').trim();
            (!text.is_empty()).then(|| text.to_owned())
        })
    };
    value("PRETTY_NAME").or_else(|| {
        let name = value("NAME")?;
        Some(value("VERSION_ID").map_or(name.clone(), |version| format!("{name} {version}")))
    })
}

/// The Windows name from its registry values. `ProductName` still says Windows 10 on Windows 11,
/// so a build of 22000 or later is named Windows 11.
// Only the Windows build reads the registry; the other builds test this formatter.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn windows_label(product: &str, display_version: &str, build: &str) -> String {
    let windows_11 = build.parse::<u32>().is_ok_and(|number| number >= 22_000);
    let mut label = product.to_owned();
    if windows_11 {
        // ProductName still says Windows 10 on a Windows 11 build, so the build decides.
        label = label.replacen("Windows 10", "Windows 11", 1);
    }
    if !display_version.is_empty() {
        label.push(' ');
        label.push_str(display_version);
    }
    if !build.is_empty() {
        label.push_str(&format!(" (build {build})"));
    }
    label
}

/// The running OS's name and version, read from the OS itself.
#[cfg(target_os = "macos")]
pub(crate) fn os_name() -> String {
    let mut buffer = [0u8; 64];
    let mut length = buffer.len();
    let status =
        // SAFETY: the name is NUL-terminated, `buffer` is writable for `length` bytes, no value is set.
        unsafe {
            libc::sysctlbyname(
                c"kern.osproductversion".as_ptr(),
                buffer.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        };
    let version = (status == 0)
        .then(|| String::from_utf8_lossy(&buffer[..length.min(buffer.len())]).into_owned())
        .map(|text| text.trim_end_matches('\0').trim().to_owned())
        .filter(|text| !text.is_empty());
    version.map_or_else(|| "macOS".to_owned(), |version| format!("macOS {version}"))
}

/// The running OS's name and version, read from the OS itself.
#[cfg(target_os = "windows")]
pub(crate) fn os_name() -> String {
    use windows::core::w;
    let product = windows_registry_text(w!("ProductName")).unwrap_or_else(|| "Windows".into());
    let display_version = windows_registry_text(w!("DisplayVersion")).unwrap_or_default();
    let build = windows_registry_text(w!("CurrentBuildNumber")).unwrap_or_default();
    windows_label(&product, &display_version, &build)
}

/// One string value under `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`.
#[cfg(target_os = "windows")]
fn windows_registry_text(name: windows::core::PCWSTR) -> Option<String> {
    use std::ffi::c_void;
    use windows::{
        core::w,
        Win32::{
            Foundation::ERROR_SUCCESS,
            System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ},
        },
    };
    let mut value = vec![0u16; 256];
    let mut bytes = u32::try_from(value.len() * 2).ok()?;
    let status =
        // SAFETY: `value` is writable for `bytes` bytes, and `bytes` is writable output storage.
        unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
                name,
                RRF_RT_REG_SZ,
                None,
                Some(value.as_mut_ptr().cast::<c_void>()),
                Some(&mut bytes),
            )
        };
    if status != ERROR_SUCCESS {
        // When: `status` is not ERROR_SUCCESS, the value is missing or too long, so it is not shown.
        return None;
    }
    let units = (bytes as usize / 2).min(value.len());
    let text = String::from_utf16_lossy(&value[..units]);
    let text = text.trim_end_matches('\0').trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The running OS's name and version, read from the OS itself.
#[cfg(target_os = "linux")]
pub(crate) fn os_name() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| linux_label(&text))
        .unwrap_or_else(|| "Linux".to_owned())
}

/// The running OS's name and version, read from the OS itself.
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub(crate) fn os_name() -> String {
    std::env::consts::OS.to_owned()
}

#[cfg(test)]
#[path = "about_tests.rs"]
mod about_tests;
