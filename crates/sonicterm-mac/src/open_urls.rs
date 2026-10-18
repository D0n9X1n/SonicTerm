//! The macOS `kAEGetURL` handler: turn an `ssh:` or `x-man-page:` link into a tab.
//!
//! A validated link runs its fixed system client in `$HOME` with each field as
//! its own argument. A rejected link, or one whose client is missing, opens no
//! tab of its own: it queues a notice that names the scheme and failing field,
//! never the link's text. On a cold start the notice waits until the first window
//! exists.

use std::path::{Path, PathBuf};

use objc2_core_services::keyDirectObject;
use objc2_foundation::NSAppleEventDescriptor;
use sonicterm_app::launch_queue::{self, LaunchRequest};
use sonicterm_cfg::launch_url::{parse_terminal_link, LinkScheme};
use sonicterm_types::{CommandLaunch, CwdSource, LaunchOrigin, TerminalLaunch};

/// `kInternetEventClass` (`'GURL'`); `objc2-core-services` 0.3 does not export it.
pub(crate) const INTERNET_EVENT_CLASS: u32 = u32::from_be_bytes(*b"GURL");
/// `kAEGetURL` (`'GURL'`); `objc2-core-services` 0.3 does not export it.
pub(crate) const GET_URL_EVENT_ID: u32 = u32::from_be_bytes(*b"GURL");

/// The link text an Apple Event carries in its direct object, if any.
pub(crate) fn link_from_event(event: &NSAppleEventDescriptor) -> Option<String> {
    let descriptor = event.paramDescriptorForKeyword(keyDirectObject)?;
    descriptor.stringValue().map(|text| text.to_string())
}

/// Turn one link into the requests to queue: a terminal launch, or one notice.
///
/// `client_exists` reports whether a client path exists; it is injected so tests
/// need not depend on the host's `/usr/bin`. Logs carry only the scheme, the
/// failing field and a fixed reason.
pub(crate) fn requests_for_link(
    link: &str,
    client_exists: impl Fn(&Path) -> bool,
    home: Option<PathBuf>,
) -> Vec<LaunchRequest> {
    let parsed = match parse_terminal_link(link) {
        Ok(parsed) => parsed,
        Err(rejection) => {
            // When: `parse_terminal_link` returns Err(rejection), which holds no link value to leak.
            tracing::warn!(
                target: "sonicterm_mac::open_url",
                scheme = rejection.scheme_name(),
                field = rejection.field.as_str(),
                reason = rejection.reason,
                "terminal link rejected"
            );
            return vec![LaunchRequest::Notice(rejection.notice())];
        }
    };
    let scheme = parsed.scheme();
    if !client_exists(Path::new(scheme.client_path())) {
        // When: `client_exists` is false for the scheme's fixed client, nothing may stand in for it.
        tracing::warn!(
            target: "sonicterm_mac::open_url",
            scheme = scheme.as_str(),
            client = scheme.client_name(),
            reason = "client_missing",
            "terminal link client is missing"
        );
        return vec![LaunchRequest::Notice(missing_client_notice(scheme))];
    }
    let cwd_source = home.as_ref().map(|_| CwdSource::Home);
    vec![LaunchRequest::Terminal(TerminalLaunch {
        command: Some(CommandLaunch {
            program: scheme.client_path().into(),
            args: parsed.args().into_iter().map(Into::into).collect(),
        }),
        cwd: home,
        cwd_source,
        title: None,
        hold: false,
        origin: LaunchOrigin::Url { scheme: scheme.as_str() },
    })]
}

/// The notice for a link whose system client is not installed.
fn missing_client_notice(scheme: LinkScheme) -> String {
    format!(
        "Link not opened: the {} link needs {}, which is not installed.",
        scheme.as_str(),
        scheme.client_path()
    )
}

/// Handle one `kAEGetURL` event: parse its link and queue the result.
pub(crate) fn handle_get_url(event: &NSAppleEventDescriptor) {
    let Some(link) = link_from_event(event) else {
        // When: `link_from_event` finds no string, the event names no link to open.
        tracing::warn!(target: "sonicterm_mac::open_url", reason = "no_link", "terminal link rejected");
        return;
    };
    let requests = requests_for_link(&link, Path::exists, sonicterm_app::launch_cli::home_dir());
    let _ = launch_queue::push_requests(requests);
}

#[cfg(test)]
#[path = "open_urls_tests.rs"]
mod open_urls_tests;
