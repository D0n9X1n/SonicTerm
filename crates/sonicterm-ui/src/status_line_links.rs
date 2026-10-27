//! Safe link labels, authority comparison, and pointer-target status formatting.

use super::*;

/// What a click on a local path does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathAction {
    /// The file is selected in its folder.
    Reveal,
    /// The folder is opened.
    OpenFolder,
}

/// What link or path detection found under the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkInput {
    /// A URL: its destination host and the rest, without the scheme.
    Url {
        /// Destination host (protected when narrowing).
        host: String,
        /// Path, query and fragment (shortened first).
        rest: String,
    },
    /// An OSC 8 URL whose visible text could not be read whole, so it was not compared.
    UrlUnchecked {
        /// Destination host (protected when narrowing).
        host: String,
        /// Path, query and fragment (shortened first).
        rest: String,
    },
    /// An OSC 8 link whose visible text names a different host.
    Mismatch {
        /// Where the link really goes.
        target_host: String,
        /// The host its visible text shows.
        label_host: String,
    },
    /// A local path and what a click does.
    Path {
        /// Path for display (`~` for home).
        path: String,
        /// What the click does.
        action: PathAction,
    },
    /// A local path still being checked.
    Checking {
        /// Path for display.
        path: String,
    },
    /// A local path that does not exist.
    NotFound {
        /// Path for display.
        path: String,
    },
    /// A local path that cannot be opened, and why.
    Blocked {
        /// Path for display.
        path: String,
        /// Localized reason.
        reason: String,
    },
}

/// `url` split into destination host and rest, scheme removed, both sanitized.
fn split_url(url: &str) -> (String, String) {
    let bare = url.split_once("://").map_or(url, |(_, rest)| rest);
    let (host, rest) = bare.find('/').map_or((bare, ""), |slash| bare.split_at(slash));
    (sanitize_link_text(host), sanitize_link_text(rest))
}

impl LinkInput {
    /// A URL split into destination host and rest, scheme removed, both sanitized.
    pub fn from_url(url: &str) -> Self {
        let (host, rest) = split_url(url);
        Self::Url { host, rest }
    }

    /// The link for a hovered target `url` and, for an OSC 8 link, its visible text and whether
    /// that text was read whole. A label that was not read whole is never reported as matching.
    pub fn from_hover(url: &str, label: Option<(&str, bool)>) -> Self {
        let checked = label.map(|(text, complete)| check_link_label(text, complete, url));
        match checked {
            Some(LinkLabelCheck::Mismatch { label_host }) => Self::Mismatch {
                target_host: url_authority(url).map_or_else(
                    || split_url(url).0,
                    |authority| sanitize_link_text(&authority.host),
                ),
                label_host: sanitize_link_text(&label_host),
            },
            Some(LinkLabelCheck::NotChecked) => {
                let (host, rest) = split_url(url);
                Self::UrlUnchecked { host, rest }
            }
            Some(LinkLabelCheck::Plain | LinkLabelCheck::Matches) | None => Self::from_url(url),
        }
    }
}

/// The host part of a URL or bare host name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlAuthority {
    /// Host, lowercased, without a trailing dot or a port.
    pub host: String,
    /// Userinfo before `@`, which makes the visible host misleading.
    pub userinfo: Option<String>,
}

/// The authority of `text` when it is a URL with a host or a bare host name such as
/// `example.com/a`; `None` for plain words, paths without a host, and `mailto:` style URIs.
pub fn url_authority(text: &str) -> Option<UrlAuthority> {
    let text = text.trim();
    let (has_scheme, rest) = text.split_once("://").map_or((false, text), |(_, rest)| (true, rest));
    let opaque_scheme = !has_scheme
        && text.split_once(':').is_some_and(|(scheme, after)| {
            !scheme.contains('/') && !after.starts_with(|next: char| next.is_ascii_digit())
        });
    if opaque_scheme {
        // When: `opaque_scheme` (`mailto:`, `tel:`) has no `//`, the text names no host.
        return None;
    }
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let (userinfo, host_port) = authority
        .rsplit_once('@')
        .map_or((None, authority), |(userinfo, host_port)| (Some(userinfo.to_string()), host_port));
    let host = match host_port.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|digit| digit.is_ascii_digit()) => host,
        _ => host_port,
    };
    let host = host.trim_end_matches('.').to_lowercase();
    let top_label = host.rsplit('.').next().unwrap_or_default();
    // A bare host needs a dot and an alphabetic top-level label, so `docs` or `v1.2` are words.
    let bare_host =
        host.contains('.') && top_label.len() >= 2 && top_label.chars().all(char::is_alphabetic);
    let plausible =
        !host.is_empty() && !host.contains(char::is_whitespace) && (has_scheme || bare_host);
    plausible.then_some(UrlAuthority { host, userinfo })
}

/// How an OSC 8 link's visible text relates to where it goes. A heuristic, not spoof protection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkLabelCheck {
    /// The text names no host to compare, such as `Docs`.
    Plain,
    /// The text names the destination's host.
    Matches,
    /// The text names another host, or userinfo makes a host misleading; the misleading host.
    Mismatch {
        /// The host the text (or the userinfo) shows.
        label_host: String,
    },
    /// The text was not read whole (cut by the view edge, the span cap or the byte cap).
    NotChecked,
}

/// Compare an OSC 8 link's visible `label` with its `target`. A `label` that is not `complete`
/// is never classified as matching.
pub fn check_link_label(label: &str, complete: bool, target: &str) -> LinkLabelCheck {
    if !complete {
        // When: `complete` is false, a truncated label could hide a different host.
        return LinkLabelCheck::NotChecked;
    }
    let Some(destination) = url_authority(target) else {
        // When: `target` has no host, there is nothing a label could misrepresent.
        return LinkLabelCheck::Plain;
    };
    let shown = url_authority(label);
    let misleading = shown
        .as_ref()
        .and_then(|shown| shown.userinfo.clone())
        .or_else(|| destination.userinfo.clone());
    if let Some(label_host) = misleading {
        // When: `misleading` userinfo is present, the host a reader sees is not the destination.
        return LinkLabelCheck::Mismatch { label_host };
    }
    match shown {
        Some(shown) if shown.host != destination.host => {
            LinkLabelCheck::Mismatch { label_host: shown.host }
        }
        Some(_) => LinkLabelCheck::Matches,
        None => LinkLabelCheck::Plain,
    }
}

/// Sanitize link or path text with the link caps.
pub fn sanitize_link_text(text: &str) -> String {
    sanitize_display_text(
        text,
        SanitizePolicy::Ordinary,
        SanitizeLimits {
            max_bytes: LINK_MAX_BYTES,
            max_columns: LINK_MAX_COLUMNS,
            collapse_whitespace: true,
        },
    )
}

/// The link segment: the URL, a host mismatch, or a local path's state.
pub(super) fn format_link(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let link = inputs.link.as_ref()?;
    let separator = symbol(env.icons, " · ", " - ");
    let detail = |key: &str, tone: StatusTone| {
        [
            StatusPart::new(separator, tone).droppable(),
            StatusPart::new(env.i18n.translate(key), tone).droppable(),
        ]
    };
    let path_name = env.i18n.translate("status-name-path");
    let (parts, protected, spoken) = match link {
        LinkInput::Url { host, rest } => {
            let mut parts = vec![
                name_part(env, "status-name-link"),
                StatusPart::new(host.clone(), StatusTone::Value),
            ];
            if !rest.is_empty() {
                parts.push(StatusPart::new(rest.clone(), StatusTone::Value).ellipsized());
            }
            (parts, true, format!("{host}{rest}"))
        }
        LinkInput::UrlUnchecked { host, rest } => {
            let mut parts = vec![
                name_part(env, "status-name-link"),
                StatusPart::new(host.clone(), StatusTone::Value),
            ];
            if !rest.is_empty() {
                parts.push(StatusPart::new(rest.clone(), StatusTone::Value).ellipsized());
            }
            parts.extend(detail("status-value-label-not-checked", StatusTone::Name));
            let note = env.i18n.translate("status-value-label-not-checked");
            (parts, true, format!("{host}{rest}, {note}"))
        }
        LinkInput::Mismatch { target_host, label_host } => (
            vec![
                name_part(env, "status-name-link"),
                StatusPart::new(symbol(env.icons, "⚠ ", "! "), StatusTone::Warn),
                StatusPart::new(target_host.clone(), StatusTone::Warn),
                StatusPart::new(symbol(env.icons, " ≠ ", " != "), StatusTone::Warn).droppable(),
                StatusPart::new(label_host.clone(), StatusTone::Warn).ellipsized().droppable(),
            ],
            true,
            format!("{target_host}, {label_host}"),
        ),
        LinkInput::Path { path, action } => {
            let action_key = match action {
                PathAction::Reveal => "status-value-reveal",
                PathAction::OpenFolder => "status-value-open-folder",
            };
            let mut parts = vec![
                name_part(env, "status-name-path"),
                StatusPart::new(path.clone(), StatusTone::Value).ellipsized(),
            ];
            parts.extend(detail(action_key, StatusTone::Name));
            (parts, false, format!("{path_name} {path}, {}", env.i18n.translate(action_key)))
        }
        LinkInput::Checking { path } => {
            let mut parts = vec![
                name_part(env, "status-name-path"),
                StatusPart::new(path.clone(), StatusTone::Name).ellipsized(),
            ];
            parts.extend(detail("status-value-checking", StatusTone::Name));
            (
                parts,
                false,
                format!("{path_name} {path}, {}", env.i18n.translate("status-value-checking")),
            )
        }
        LinkInput::NotFound { path } => (
            vec![
                name_part(env, "status-name-path"),
                StatusPart::new(symbol(env.icons, "✗ ", "x "), StatusTone::Error),
                StatusPart::new(path.clone(), StatusTone::Value).ellipsized(),
                StatusPart::new(separator, StatusTone::Error),
                StatusPart::new(env.i18n.translate("status-value-not-found"), StatusTone::Error),
            ],
            false,
            format!("{path_name} {path}, {}", env.i18n.translate("status-value-not-found")),
        ),
        LinkInput::Blocked { path, reason } => (
            vec![
                name_part(env, "status-name-path"),
                StatusPart::new(symbol(env.icons, "⚠ ", "! "), StatusTone::Warn),
                StatusPart::new(path.clone(), StatusTone::Value).ellipsized(),
                StatusPart::new(separator, StatusTone::Warn),
                StatusPart::new(env.i18n.translate("status-value-blocked"), StatusTone::Warn),
                StatusPart::new(format!(": {reason}"), StatusTone::Warn).ellipsized().droppable(),
            ],
            false,
            format!("{path_name} {path}, {}, {reason}", env.i18n.translate("status-value-blocked")),
        ),
    };
    Some(FormattedSegment { protected, ..FormattedSegment::plain(parts, spoken) })
}

#[cfg(test)]
#[path = "status_line_links_tests.rs"]
mod status_line_links_tests;
