//! The macOS branch of the default-handler command: Launch Services through `NSWorkspace`.
//!
//! The plan reads the claims from the installed bundle's own `Info.plist`
//! (`CFBundleDocumentTypes` content types and `CFBundleURLTypes` schemes), so the list
//! cannot drift from what the bundle declares. It refuses when the process is not the
//! installed `com.d0n9x1n.sonicterm` bundle: a `cargo run` build, a translocated or
//! disk-image copy (both run from a read-only volume), or a bundle Launch Services has
//! not registered. Apply calls `setDefaultApplicationAtURL:toOpenContentType:` and
//! `…toOpenURLsWithScheme:` on the main thread; each completion handler reports through
//! the result sink, which posts to the event loop. Nothing here waits.

#[cfg(target_os = "macos")]
use std::path::Path;

use sonicterm_app::default_handlers::{
    HandlerItem, HandlerKind, HandlerOutcome, HandlerPlan, PlanMode,
};

/// The bundle identifier the DMG's `Info.plist` declares.
pub(crate) const BUNDLE_IDENTIFIER: &str = "com.d0n9x1n.sonicterm";

/// What a bundle's `Info.plist` claims.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BundleClaims {
    /// `CFBundleIdentifier`.
    pub(crate) identifier: Option<String>,
    /// Every `LSItemContentTypes` entry of `CFBundleDocumentTypes`, in order, without repeats.
    pub(crate) content_types: Vec<String>,
    /// Every `CFBundleURLSchemes` entry of `CFBundleURLTypes`, in order, without repeats.
    pub(crate) schemes: Vec<String>,
}

/// The element children of `node`.
fn element_children<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
) -> Vec<roxmltree::Node<'a, 'input>> {
    node.children().filter(roxmltree::Node::is_element).collect()
}

/// The value element following `<key>key</key>` in a plist `dict`.
fn dict_value<'a, 'input>(
    dict: roxmltree::Node<'a, 'input>,
    key: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    let children = element_children(dict);
    let index =
        children.iter().position(|child| child.has_tag_name("key") && child.text() == Some(key))?;
    children.get(index + 1).copied()
}

/// The text of each `<string>` in the array stored under `key` in `dict`.
fn string_array(dict: roxmltree::Node<'_, '_>, key: &str) -> Vec<String> {
    dict_value(dict, key)
        .filter(|value| value.has_tag_name("array"))
        .map(|array| {
            element_children(array)
                .into_iter()
                .filter(|item| item.has_tag_name("string"))
                .filter_map(|item| item.text().map(str::trim).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Gather the `key` string arrays of every dict in the array under `types_key`.
fn claimed_strings(root: roxmltree::Node<'_, '_>, types_key: &str, key: &str) -> Vec<String> {
    let mut claimed: Vec<String> = Vec::new();
    let Some(types) = dict_value(root, types_key).filter(|value| value.has_tag_name("array"))
    else {
        // When: `dict_value` finds no `types_key` array, the bundle claims none of these.
        return claimed;
    };
    for entry in element_children(types).into_iter().filter(|entry| entry.has_tag_name("dict")) {
        for value in string_array(entry, key) {
            if !claimed.contains(&value) {
                claimed.push(value);
            }
        }
    }
    claimed
}

/// Read the identifier, content types and URL schemes an XML `Info.plist` claims.
///
/// The DOCTYPE line is dropped first, because the XML reader refuses an external DTD.
pub(crate) fn claims_from_info_plist(xml: &str) -> Result<BundleClaims, String> {
    let without_doctype: String = xml
        .lines()
        .filter(|line| !line.trim_start().starts_with("<!DOCTYPE"))
        .collect::<Vec<_>>()
        .join("\n");
    let document = roxmltree::Document::parse(&without_doctype)
        .map_err(|_| "SonicTerm's Info.plist is not readable XML.".to_string())?;
    let root = document
        .descendants()
        .find(|node| node.has_tag_name("plist"))
        .and_then(|plist| {
            element_children(plist).into_iter().find(|node| node.has_tag_name("dict"))
        })
        .ok_or_else(|| "SonicTerm's Info.plist has no top-level dictionary.".to_string())?;
    Ok(BundleClaims {
        identifier: dict_value(root, "CFBundleIdentifier")
            .and_then(|value| value.text())
            .map(|text| text.trim().to_string()),
        content_types: claimed_strings(root, "CFBundleDocumentTypes", "LSItemContentTypes"),
        schemes: claimed_strings(root, "CFBundleURLTypes", "CFBundleURLSchemes"),
    })
}

/// What the running process knows about its own bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BundleFacts {
    /// The main bundle's path ends in `.app`.
    pub(crate) is_app_bundle: bool,
    /// `CFBundleIdentifier` from that bundle's `Info.plist`.
    pub(crate) identifier: Option<String>,
    /// The bundle runs from a read-only volume: a mounted disk image or App Translocation.
    pub(crate) read_only_volume: bool,
    /// The bundle path has an `AppTranslocation` component.
    pub(crate) translocated: bool,
    /// Launch Services lists this exact bundle for the identifier.
    pub(crate) registered: bool,
}

/// Why the command must not change anything for this bundle, if it must not.
pub(crate) fn bundle_refusal(facts: &BundleFacts) -> Option<String> {
    if !facts.is_app_bundle || facts.identifier.as_deref() != Some(BUNDLE_IDENTIFIER) {
        // When: `is_app_bundle` is false or `identifier` differs, this is not the installed app.
        return Some(
            "Missing bundle: this SonicTerm is not running from SonicTerm.app (for example a cargo run \
             build), so there is nothing to set as the default. Nothing was changed."
                .to_string(),
        );
    }
    if facts.translocated || facts.read_only_volume {
        // When: `translocated` or `read_only_volume` holds, macOS would record a temporary copy.
        return Some(
            "SonicTerm is running from a disk image or a translocated copy. Move SonicTerm.app to \
             /Applications, open it from there, and try again. Nothing was changed."
                .to_string(),
        );
    }
    if !facts.registered {
        // When: `registered` is false, Launch Services would not resolve the handler to this copy.
        return Some(
            "Launch Services has not registered this copy of SonicTerm. Open it once from \
             /Applications and try again. Nothing was changed."
                .to_string(),
        );
    }
    None
}

/// Whether a bundle path lies inside an App Translocation mount.
pub(crate) fn is_translocated_path(path: &str) -> bool {
    path.split('/').any(|component| component == "AppTranslocation")
}

/// The confirmation plan for `claims`: every content type, then every scheme.
pub(crate) fn plan_for_claims(claims: &BundleClaims) -> Result<HandlerPlan, String> {
    let items: Vec<HandlerItem> = claims
        .content_types
        .iter()
        .map(|name| HandlerItem::new(HandlerKind::ContentType, name.as_str()))
        .chain(
            claims
                .schemes
                .iter()
                .map(|name| HandlerItem::new(HandlerKind::UrlScheme, name.as_str())),
        )
        .collect();
    if items.is_empty() {
        // When: `items` is empty, the bundle declares nothing for SonicTerm to claim.
        return Err("SonicTerm's Info.plist claims no file type or link scheme.".to_string());
    }
    Ok(HandlerPlan {
        mode: PlanMode::Change,
        items,
        confirm_notes: vec!["macOS may ask you to confirm some of these.".to_string()],
        report_notes: vec![
            "Scripts open as drafts; only verified Mach-O executables run.".to_string()
        ],
    })
}

/// `NSCocoaErrorDomain`'s user-cancelled code.
const COCOA_USER_CANCELLED: i64 = 3072;
/// The Carbon `userCanceledErr` OSStatus.
const OS_STATUS_USER_CANCELED: i64 = -128;
/// The `permErr` and `afpAccessDenied` OSStatus codes a refused change can carry.
const OS_STATUS_DENIED: [i64; 2] = [-54, -5000];

/// Classify a completion handler's error; `None` means the change succeeded.
///
/// The codes for a declined consent prompt are not documented, so a cancellation code
/// reads as cancelled, a permission code as denied, and anything else as failed with its
/// domain and code.
pub(crate) fn outcome_for_error(error: Option<(&str, i64)>) -> HandlerOutcome {
    match error {
        None => HandlerOutcome::Changed,
        Some(("NSCocoaErrorDomain", COCOA_USER_CANCELLED))
        | Some(("NSOSStatusErrorDomain", OS_STATUS_USER_CANCELED)) => HandlerOutcome::Cancelled,
        Some(("NSOSStatusErrorDomain", code)) if OS_STATUS_DENIED.contains(&code) => {
            HandlerOutcome::Denied
        }
        Some((domain, code)) => HandlerOutcome::Failed(format!("{domain} {code}")),
    }
}

/// Whether `bundle_path` is among the bundle paths Launch Services lists for the identifier.
pub(crate) fn is_registered(bundle_path: &str, registered_paths: &[String]) -> bool {
    let wanted = bundle_path.trim_end_matches('/');
    registered_paths.iter().any(|path| path.trim_end_matches('/') == wanted)
}

/// Whether `path` lies on a volume mounted read-only; false when it cannot be checked.
#[cfg(target_os = "macos")]
pub(crate) fn is_read_only_volume(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        // When: `CString::new` rejects an interior NUL, there is no path to ask about.
        return false;
    };
    let mut stats = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    let status =
        // SAFETY: `c_path` is a NUL-terminated string and `stats` is writable storage for one statfs.
        unsafe { libc::statfs(c_path.as_ptr(), stats.as_mut_ptr()) };
    if status != 0 {
        // When: `status` is nonzero, statfs failed and the volume's mode is unknown.
        return false;
    }
    let stats =
        // SAFETY: statfs returned 0, so it filled `stats` completely.
        unsafe { stats.assume_init() };
    stats.f_flags & libc::MNT_RDONLY as u32 != 0
}

#[cfg(target_os = "macos")]
pub(crate) use native::MacDefaultHandlers;

#[cfg(target_os = "macos")]
mod native {
    use std::path::Path;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSBundle, NSError, NSString, NSURL};
    use sonicterm_app::default_handlers::{
        DefaultHandlerBackend, HandlerKind, HandlerOutcome, HandlerPlan, HandlerResultSink,
    };

    use super::{
        bundle_refusal, claims_from_info_plist, is_read_only_volume, is_registered,
        is_translocated_path, outcome_for_error, plan_for_claims, BundleFacts, BUNDLE_IDENTIFIER,
    };

    // `UTType` lives in UniformTypeIdentifiers; linking it keeps the class resolvable at runtime.
    #[link(name = "UniformTypeIdentifiers", kind = "framework")]
    extern "C" {}

    /// The `UTType` for `identifier`, looked up at runtime; `None` when it is unknown.
    pub(super) fn content_type(identifier: &str) -> Option<Retained<AnyObject>> {
        let class = AnyClass::get(c"UTType")?;
        let name = NSString::from_str(identifier);
        // SAFETY: `+[UTType typeWithIdentifier:]` takes one NSString and returns an autoreleased
        // UTType or nil; msg_send retains the result.
        unsafe { objc2::msg_send![class, typeWithIdentifier: &*name] }
    }

    /// The paths of every bundle Launch Services lists for `identifier`.
    fn registered_paths(workspace: &NSWorkspace, identifier: &str) -> Vec<String> {
        let identifier = NSString::from_str(identifier);
        workspace
            .URLsForApplicationsWithBundleIdentifier(&identifier)
            .iter()
            .filter_map(|url| url.path().map(|path| path.to_string()))
            .collect()
    }

    /// The `NSWorkspace` backend the macOS palette command uses.
    #[derive(Default)]
    pub(crate) struct MacDefaultHandlers {
        /// The installed bundle the last successful plan checked; Apply uses exactly it.
        bundle: Option<Retained<NSURL>>,
    }

    impl DefaultHandlerBackend for MacDefaultHandlers {
        fn platform(&self) -> &'static str {
            "macos"
        }

        fn plan(&mut self) -> Result<HandlerPlan, String> {
            self.bundle = None;
            let bundle_url = NSBundle::mainBundle().bundleURL();
            let bundle_path = bundle_url.path().map(|path| path.to_string()).unwrap_or_default();
            let plist = Path::new(&bundle_path).join("Contents/Info.plist");
            let is_app_bundle = bundle_path.ends_with(".app");
            let claims = match std::fs::read_to_string(&plist) {
                Ok(xml) if is_app_bundle => claims_from_info_plist(&xml)?,
                _ => Default::default(),
            };
            let workspace = NSWorkspace::sharedWorkspace();
            let facts = BundleFacts {
                is_app_bundle,
                identifier: claims.identifier.clone(),
                read_only_volume: is_read_only_volume(Path::new(&bundle_path)),
                translocated: is_translocated_path(&bundle_path),
                registered: is_registered(
                    &bundle_path,
                    &registered_paths(&workspace, BUNDLE_IDENTIFIER),
                ),
            };
            if let Some(refusal) = bundle_refusal(&facts) {
                // When: `bundle_refusal` names a reason, the log records the facts but no path.
                tracing::info!(
                    target: "sonicterm_mac::default_handlers",
                    app_bundle = facts.is_app_bundle,
                    identifier_matches = facts.identifier.as_deref() == Some(BUNDLE_IDENTIFIER),
                    read_only_volume = facts.read_only_volume,
                    translocated = facts.translocated,
                    registered = facts.registered,
                    "default handlers refused for this bundle"
                );
                return Err(refusal);
            }
            let plan = plan_for_claims(&claims)?;
            self.bundle = Some(bundle_url);
            Ok(plan)
        }

        fn apply(&mut self, plan: &HandlerPlan, sink: HandlerResultSink) {
            let (Some(bundle), Some(_main_thread)) = (self.bundle.clone(), MainThreadMarker::new())
            else {
                // When: no checked `bundle` exists or this is not the main thread, nothing may run.
                for index in 0..plan.items.len() {
                    sink.report(
                        index,
                        HandlerOutcome::Failed("not ready on the main thread".into()),
                    );
                }
                return;
            };
            let workspace = NSWorkspace::sharedWorkspace();
            for (index, item) in plan.items.iter().enumerate() {
                let item_sink = sink.clone();
                let completion = RcBlock::new(move |error: *mut NSError| {
                    let error =
                        // SAFETY: AppKit passes nil or a valid NSError that lives for this call.
                        unsafe { error.as_ref() };
                    let domain = error.map(|error| error.domain().to_string());
                    let outcome = outcome_for_error(
                        domain.as_deref().zip(error.map(|error| error.code() as i64)),
                    );
                    item_sink.report(index, outcome);
                });
                match item.kind {
                    HandlerKind::UrlScheme => {
                        let scheme = NSString::from_str(&item.name);
                        // Called on the main thread; AppKit copies the block and calls it once.
                        workspace
                            .setDefaultApplicationAtURL_toOpenURLsWithScheme_completionHandler(
                                &bundle,
                                &scheme,
                                Some(&completion),
                            );
                    }
                    _ => {
                        // When: `item.kind` is a content type, resolve its UTType before the setter.
                        let Some(content_type) = content_type(&item.name) else {
                            // When: `content_type` finds no UTType, the system does not know this type.
                            sink.report(
                                index,
                                HandlerOutcome::Failed("unknown content type".into()),
                            );
                            continue;
                        };
                        let block: &block2::Block<dyn Fn(*mut NSError)> = &completion;
                        // SAFETY: `setDefaultApplicationAtURL:toOpenContentType:completionHandler:`
                        // takes an NSURL, a UTType and a block, all live here; AppKit copies the block.
                        unsafe {
                            let _: () = objc2::msg_send![
                                &*workspace,
                                setDefaultApplicationAtURL: &*bundle,
                                toOpenContentType: &*content_type,
                                completionHandler: block
                            ];
                        }
                    }
                }
            }
        }
    }

    /// Whether the UTType class this backend resolves at runtime is present.
    #[cfg(test)]
    pub(super) fn ut_type_class_present() -> bool {
        AnyClass::get(c"UTType").is_some()
    }

    /// The bundle id the Shell role of `content_type` resolves to (read only).
    #[cfg(test)]
    pub(super) fn shell_role_handler(content_type: &str) -> Option<String> {
        #[link(name = "CoreServices", kind = "framework")]
        extern "C" {
            fn LSCopyDefaultRoleHandlerForContentType(
                content_type: *const AnyObject,
                role: u32,
            ) -> *mut AnyObject;
        }
        /// `kLSRolesShell`.
        const ROLES_SHELL: u32 = 0x0000_0008;
        let name = NSString::from_str(content_type);
        let handler =
            // SAFETY: NSString is toll-free bridged to CFString; the call only reads Launch Services.
            unsafe { LSCopyDefaultRoleHandlerForContentType(Retained::as_ptr(&name).cast(), ROLES_SHELL) };
        if handler.is_null() {
            // When: `handler` is null, no default handler is recorded for the Shell role.
            return None;
        }
        let handler: Retained<NSString> =
            // SAFETY: a non-null result is a +1 CFString (bridged NSString) the caller owns.
            unsafe { Retained::from_raw(handler.cast()) }?;
        Some(handler.to_string())
    }
}

#[cfg(test)]
#[path = "default_handlers_tests.rs"]
mod default_handlers_tests;
