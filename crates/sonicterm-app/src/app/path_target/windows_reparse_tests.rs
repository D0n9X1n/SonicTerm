//! Windows path decoding and native handle-custody regression cases.

use super::super::windows_tests::*;
use super::*;

/// A file symlink on a local fixed disk, with an absolute or a relative target, classifies like the
/// file it names, and dispatch passes Explorer the file's real, link-free path, so the file is
/// selected in its real folder rather than the link's.
#[cfg(target_os = "windows")]
#[test]
fn native_symlinked_files_are_selected_in_their_real_folder() {
    let root = scratch_folder("file-symlink");
    std::fs::create_dir_all(root.join("real")).unwrap();
    std::fs::write(root.join(r"real\notes.txt"), "notes").unwrap();
    let absolute = root.join("absolute.txt");
    let relative = root.join("relative.txt");
    let created = create_symlink(&root.join(r"real\notes.txt"), &absolute, false)
        && create_symlink(Path::new(r"real\notes.txt"), &relative, false);
    if !created {
        std::fs::remove_dir_all(&root).unwrap();
        return;
    }
    for link in [&absolute, &relative] {
        let decision = classify_local_target(link);
        assert_eq!(decision, PathOpenDecision::Openable(PathKind::File), "{}", link.display());
        assert_eq!(local_target_action(decision), Some(LocalTargetAction::Reveal));
        validate_reveal_target(link, decision).unwrap();
        let mut dispatched = None;
        dispatch_held_target(link, decision, &mut NativeLinkProbe, |walked| {
            dispatched = Some(PathBuf::from(walked));
            Ok(())
        })
        .unwrap();
        assert_eq!(dispatched, Some(root.join(r"real\notes.txt")), "{}", link.display());
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// A junction to a local folder navigates like that folder: it classifies as a directory, dispatch
/// passes the shell the real folder's link-free path, and a file reached through it is selected.
#[cfg(target_os = "windows")]
#[test]
fn native_junctioned_folders_are_opened() {
    let root = scratch_folder("junction");
    std::fs::create_dir_all(root.join("real")).unwrap();
    std::fs::write(root.join(r"real\notes.txt"), "notes").unwrap();
    let junction = root.join("junction");
    create_junction(&root.join("real"), &junction);
    let decision = classify_local_target(&junction);
    assert_eq!(decision, PathOpenDecision::Openable(PathKind::Directory));
    assert_eq!(local_target_action(decision), Some(LocalTargetAction::Navigate));
    let mut dispatched = None;
    dispatch_held_target(&junction, decision, &mut NativeLinkProbe, |walked| {
        dispatched = Some(PathBuf::from(walked));
        Ok(())
    })
    .unwrap();
    assert_eq!(dispatched, Some(root.join("real")));
    let through = junction.join("notes.txt");
    let file_decision = classify_local_target(&through);
    assert_eq!(file_decision, PathOpenDecision::Openable(PathKind::File));
    validate_reveal_target(&through, file_decision).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// A symlink to a TEST-NET `\\192.0.2.1\share` path is refused from its reparse data: the walk opens
/// only local entries and never names the host, whether the link is a file, a folder, or a folder
/// on the path. Creating the links needs no network.
#[cfg(target_os = "windows")]
#[test]
fn native_unc_symlinks_are_refused_without_contacting_the_host() {
    let root = scratch_folder("unc-link");
    let share = Path::new(r"\\192.0.2.1\share");
    let file_link = root.join("share.txt");
    let folder_link = root.join("share-folder");
    let created = create_symlink(&share.join("notes.txt"), &file_link, false)
        && create_symlink(share, &folder_link, true);
    if !created {
        std::fs::remove_dir_all(&root).unwrap();
        return;
    }
    let mut probe = RecordingProbe::default();
    for path in [file_link.clone(), folder_link.clone(), folder_link.join("notes.txt")] {
        let decision = classify_windows_target_with(&path, &mut probe);
        assert_eq!(decision, PathOpenDecision::Blocked, "{}", path.display());
        assert_eq!(classify_local_target(&path), PathOpenDecision::Blocked, "{}", path.display());
    }
    assert!(probe.calls.iter().all(|call| !call.contains("192.0.2.1") && !call.contains(r"\\")));
    std::fs::remove_dir_all(root).unwrap();
}

/// A drive letter redefined to a local folder, as `subst` does, is refused after the namespace
/// query alone, with no call on the folder it names, both as the start of a path and as a link's
/// target, though Windows itself resolves the letter.
#[cfg(target_os = "windows")]
#[test]
fn native_redefined_drive_letters_are_refused_before_any_call_on_them() {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{DefineDosDeviceW, DDD_RAW_TARGET_PATH};
    let root = scratch_folder("redefined-letter");
    std::fs::create_dir_all(root.join("mapped")).unwrap();
    std::fs::write(root.join(r"mapped\notes.txt"), "notes").unwrap();
    let letter = ('D'..='Z')
        .rev()
        .find(|letter| NativeLinkProbe.dos_device(*letter).is_none())
        .expect("a free drive letter");
    let device = format!("{letter}:").encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let folder = root.join("mapped");
    let raw_folder = format!(r"\??\{}", folder.to_str().unwrap());
    let raw_target = raw_folder.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let defined =
        // SAFETY: `device` and `raw_target` are NUL-terminated UTF-16 strings that outlive the call.
        unsafe {
        DefineDosDeviceW(
            DDD_RAW_TARGET_PATH,
            PCWSTR(device.as_ptr()),
            PCWSTR(raw_target.as_ptr()),
        )
    };
    defined.unwrap();
    let guard = DefinedLetter { device, raw_target };
    let mapped = PathBuf::from(format!(r"{letter}:\notes.txt"));
    // Windows resolves the letter to the folder, so a refusal comes from the walk alone.
    assert!(mapped.is_file());
    let mut probe = RecordingProbe::default();
    assert_eq!(classify_windows_target_with(&mapped, &mut probe), PathOpenDecision::Blocked);
    assert_eq!(probe.calls, [format!("dos_device {letter}")]);
    let link = root.join("mapped-link.txt");
    if create_symlink(&mapped, &link, false) {
        let mut probe = RecordingProbe::default();
        assert_eq!(classify_windows_target_with(&link, &mut probe), PathOpenDecision::Blocked);
        assert_eq!(probe.calls.last(), Some(&format!("dos_device {letter}")));
    }
    drop(guard);
    // The exact-match removal took this test's definition off the letter.
    assert_ne!(NativeLinkProbe.dos_device(letter), Some(raw_folder));
    std::fs::remove_dir_all(root).unwrap();
}

/// A readable file whose ACL denies execute is still a file, and dispatch selects it: the walk
/// holds it with `FILE_READ_DATA`, which selection needs, rather than refusing it for the execute
/// right it never uses.
#[cfg(target_os = "windows")]
#[test]
fn native_files_that_deny_execute_are_still_selected() {
    let root = scratch_folder("execute-denied");
    let file = root.join("notes.txt");
    std::fs::write(&file, "notes").unwrap();
    let guard = DeniedRights::apply(&file, "X");
    let decision = classify_local_target(&file);
    assert_eq!(decision, PathOpenDecision::Openable(PathKind::File));
    let mut dispatched = None;
    dispatch_held_target(&file, decision, &mut NativeLinkProbe, |walked| {
        dispatched = Some(PathBuf::from(walked));
        Ok(())
    })
    .unwrap();
    assert_eq!(dispatched, Some(file));
    drop(guard);
    std::fs::remove_dir_all(root).unwrap();
}

/// A folder whose ACL denies listing is still walked: the read attempt is denied, the retry with
/// `FILE_TRAVERSE` holds the folder, and a file inside it is classified and selected.
#[cfg(target_os = "windows")]
#[test]
fn native_folders_that_deny_listing_are_still_walked() {
    let root = scratch_folder("listing-denied");
    let folder = root.join("private");
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder.join("notes.txt");
    std::fs::write(&file, "notes").unwrap();
    let guard = DeniedRights::apply(&folder, "RD");
    let decision = classify_local_target(&file);
    assert_eq!(decision, PathOpenDecision::Openable(PathKind::File));
    let mut dispatched = None;
    dispatch_held_target(&file, decision, &mut NativeLinkProbe, |walked| {
        dispatched = Some(PathBuf::from(walked));
        Ok(())
    })
    .unwrap();
    assert_eq!(dispatched, Some(file));
    drop(guard);
    std::fs::remove_dir_all(root).unwrap();
}

/// Dispatch walks the whole chain again: a file link and a folder link that named local targets
/// when probed, then were retargeted to a network share, are refused before any shell call.
#[cfg(target_os = "windows")]
#[test]
fn native_dispatch_refuses_links_retargeted_to_a_network_share() {
    let root = scratch_folder("retarget");
    std::fs::create_dir_all(root.join("real")).unwrap();
    std::fs::write(root.join(r"real\notes.txt"), "notes").unwrap();
    let file_link = root.join("notes-link.txt");
    let folder_link = root.join("folder-link");
    let created = create_symlink(&root.join(r"real\notes.txt"), &file_link, false)
        && create_symlink(&root.join("real"), &folder_link, true);
    if !created {
        std::fs::remove_dir_all(&root).unwrap();
        return;
    }
    let file_decision = classify_local_target(&file_link);
    let folder_decision = classify_local_target(&folder_link);
    assert_eq!(file_decision, PathOpenDecision::Openable(PathKind::File));
    assert_eq!(folder_decision, PathOpenDecision::Openable(PathKind::Directory));
    // Removing a symlink removes only the link, never its target.
    std::fs::remove_file(&file_link).unwrap();
    std::fs::remove_dir(&folder_link).unwrap();
    let share = Path::new(r"\\192.0.2.1\share");
    assert!(create_symlink(&share.join("notes.txt"), &file_link, false));
    assert!(create_symlink(share, &folder_link, true));
    for (link, decision) in [(&file_link, file_decision), (&folder_link, folder_decision)] {
        let refused = dispatch_held_target(link, decision, &mut NativeLinkProbe, |_| {
            unreachable!("a retargeted link never reaches the shell")
        });
        assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }
    let reveal = validate_reveal_target(&file_link, file_decision).unwrap_err();
    assert_eq!(reveal.kind(), io::ErrorKind::PermissionDenied);
    std::fs::remove_dir_all(root).unwrap();
}

/// While a custody walk holds `parent\child.txt`, neither part can be renamed or deleted, since
/// every open leaves out `FILE_SHARE_DELETE`, and `parent` cannot be turned into a junction, since
/// it is not empty; once the walk's handles are released, the same rename and delete succeed.
#[cfg(target_os = "windows")]
#[test]
fn native_custody_keeps_held_parts_from_being_renamed_deleted_or_linked() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::IO::DeviceIoControl;
    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const ERROR_DIR_NOT_EMPTY: u32 = 145;
    let root = scratch_folder("custody");
    let parent = root.join("parent");
    let child = parent.join("child.txt");
    std::fs::create_dir_all(&parent).unwrap();
    std::fs::write(&child, "child").unwrap();
    let held = hold_windows_target(&child, &mut NativeLinkProbe)
        .unwrap_or_else(|decision| panic!("custody walk refused: {decision:?}"));
    assert_eq!(held.kind, PathKind::File);
    assert!(std::fs::rename(&parent, root.join("renamed")).is_err());
    assert!(std::fs::remove_file(&child).is_err());
    assert!(parent.is_dir() && child.is_file());
    let folder = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&parent)
        .unwrap();
    let junction = reparse_buffer(IO_REPARSE_TAG_MOUNT_POINT, None, r"\??\C:\", r"C:\");
    let length = u32::try_from(junction.len()).unwrap();
    let mut returned = 0_u32;
    let linked =
        // SAFETY: `folder` is a live handle; `junction` holds `length` readable bytes and `returned` is a writable local.
        unsafe {
        DeviceIoControl(
            HANDLE(folder.as_raw_handle()),
            FSCTL_SET_REPARSE_POINT,
            Some(junction.as_ptr().cast()),
            length,
            None,
            0,
            Some(std::ptr::from_mut(&mut returned)),
            None,
        )
    };
    let error = linked.unwrap_err();
    assert_eq!(error.code(), windows::core::HRESULT::from_win32(ERROR_DIR_NOT_EMPTY));
    drop(folder);
    drop(held);
    std::fs::remove_file(&child).unwrap();
    std::fs::rename(&parent, root.join("renamed")).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Native manual probe uses production validation/dispatch; Explorer selection is inspected by the caller.
#[cfg(target_os = "windows")]
#[test]
#[ignore = "opens Explorer for an explicitly supplied native test fixture"]
fn reveal_file_in_explorer_native_probe() {
    let path =
        PathBuf::from(std::env::var_os("SONICTERM_REVEAL_PROBE_FILE").expect("explicit fixture"));
    let decision = classify_local_target(&path);
    assert_eq!(decision, PathOpenDecision::Openable(PathKind::File));
    open_path(&path, decision).expect("native Explorer selection request");
}

/// A dedicated process runs real native input and path workers with scratch state; the driver verifies Explorer selection.
#[cfg(target_os = "windows")]
#[test]
#[ignore = "requires an external native pointer driver and explicit scratch directory"]
fn structural_paths_native_interaction() {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::{
        application::ApplicationHandler,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
    };
    struct NativeProbe {
        app: App,
        root: PathBuf,
        started: std::time::Instant,
        sequence: u64,
        input_sequence: u64,
        last_pointer_event: serde_json::Value,
    }
    impl ApplicationHandler<super::super::super::super::UserEvent> for NativeProbe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            self.app.resumed(event_loop);
        }
        fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
            self.app.new_events(event_loop, cause);
        }
        fn user_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            event: super::super::super::super::UserEvent,
        ) {
            self.app.user_event(event_loop, event);
        }
        fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
            if matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::CursorEntered { .. }
                    | WindowEvent::CursorLeft { .. }
                    | WindowEvent::ModifiersChanged(_)
                    | WindowEvent::Focused(_)
                    | WindowEvent::MouseInput { .. }
            ) {
                self.input_sequence += 1;
                self.last_pointer_event = serde_json::json!({
                    "window": format!("{id:?}"),
                    "event": format!("{event:?}"),
                    "sequence": self.input_sequence,
                });
            }
            self.app.window_event(event_loop, id, event);
        }
        fn device_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            id: winit::event::DeviceId,
            event: winit::event::DeviceEvent,
        ) {
            self.app.device_event(event_loop, id, event);
        }
        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            self.app.about_to_wait(event_loop);
            let native_windows = self.app.windows.iter().filter_map(|(id, state)| {
                let window = state.window.as_ref()?;
                let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else { return None };
                let active_pane = state.tab_states.get(state.tabs.active_index())?.active_pane;
                let pane = state.panes.get(&active_pane)?;
                let parser = pane.parser.try_lock()?;
                let rows = parser.grid().rows_iter().map(|row| row.iter().map(|cell| cell.ch).collect::<String>()).collect::<Vec<_>>();
                Some(serde_json::json!({"hwnd":handle.hwnd.get(),"main":Some(*id)==self.app.main_window_id,"width":window.inner_size().width,"height":window.inner_size().height,"scale":window.scale_factor(),"tabs":state.tabs.len(),"rows":rows,"hidden":state.hidden}))
            }).collect::<Vec<_>>();
            std::fs::write(
                self.root.join("windows.json"),
                serde_json::to_vec(&native_windows).unwrap(),
            )
            .unwrap();
            if let Some(id) = self.app.main_window_id {
                let window = &self.app.windows[&id];
                let tab = &window.tab_states[window.tabs.active_index()];
                if let (Some(native), Some(renderer), Some(pane)) =
                    (&window.window, &window.renderer, window.panes.get(&tab.active_pane))
                {
                    if let Some(parser) = pane.parser.try_lock() {
                        let RawWindowHandle::Win32(handle) =
                            native.window_handle().unwrap().as_raw()
                        else {
                            panic!("Windows handle")
                        };
                        let rows = parser
                            .grid()
                            .rows_iter()
                            .map(|row| row.iter().map(|cell| cell.ch).collect::<String>())
                            .collect::<Vec<_>>();
                        let (cell_width, cell_height) = renderer.cell_size();
                        let pane_id = window.tab_states[window.tabs.active_index()].active_pane;
                        let origin = renderer.pane_grid_origin(pane_id);
                        let pointer_cell = renderer.pixel_to_pane_cell(
                            window.cursor_pos.0 as f32,
                            window.cursor_pos.1 as f32,
                        );
                        // Observe the held parser without advancing probes or granting fresh authorization.
                        let fresh = pointer_cell
                            .filter(|(pointed_pane, _, _)| *pointed_pane == pane_id)
                            .and_then(|(_, row, col)| {
                                self.app.cell_target_from_parser(
                                    id,
                                    pane_id,
                                    row,
                                    col,
                                    &parser,
                                    pane.viewport_top_abs,
                                )
                            });
                        let probe = &window.path_probe;
                        let fresh_key = fresh.as_ref().and_then(|target| match &target.target {
                            ResolvedCellTarget::Path(key) => Some(key),
                            _ => None,
                        });
                        let current_matches =
                            fresh_key.is_some_and(|key| probe.current.as_ref() == Some(key));
                        let settled = if fresh_key.is_some() {
                            current_matches
                                && probe.pending_result.is_none()
                                && (probe.selection.is_some() || probe.failure.is_some())
                        } else {
                            fresh.is_none()
                                && pointer_cell.is_some_and(|(pane, _, _)| pane == pane_id)
                                && probe.current.is_none()
                                && probe.pending_result.is_none()
                        };
                        let mut native_cursor = windows::Win32::Foundation::POINT::default();
                        let hwnd = windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut _);
                        let (native_cursor_screen, native_cursor_client, foreground) =
                            // SAFETY: native keeps hwnd live; native_cursor is writable and other handles are only compared.
                            unsafe {
                                use windows::Win32::{
                                    Graphics::Gdi::ScreenToClient,
                                    UI::WindowsAndMessaging::{GetCursorPos, GetForegroundWindow},
                                };
                                let screen = GetCursorPos(&mut native_cursor)
                                    .ok()
                                    .map(|()| [native_cursor.x, native_cursor.y]);
                                let client = screen.and_then(|_| {
                                    ScreenToClient(hwnd, &mut native_cursor)
                                        .as_bool()
                                        .then_some([native_cursor.x, native_cursor.y])
                                });
                                (screen, client, GetForegroundWindow() == hwnd)
                            };
                        self.sequence += 1;
                        let report = serde_json::json!({
                            "sequence": self.sequence,
                            "input_sequence": self.input_sequence,
                            "last_pointer_event": self.last_pointer_event,
                            "cursor_pos": [window.cursor_pos.0, window.cursor_pos.1],
                            "native_cursor_screen": native_cursor_screen,
                            "native_cursor_client": native_cursor_client,
                            "foreground": foreground,
                            "open_modifier": self.app.open_modifier_held(id),
                            "probe": {
                                "epoch": probe.epoch.0,
                                "pointed": probe.current.as_ref().map(|key| [key.pointed.row, u64::from(key.pointed.col)]),
                                "current_matches": current_matches,
                                "settled": settled,
                                "pending_result": probe.pending_result.is_some(),
                                "failure": probe.failure,
                                "selection": probe.selection.as_ref().map(|selection| selection.candidate.resolved_path.to_string_lossy()),
                                "fresh_kind": match fresh.as_ref().map(|target| &target.target) {
                                    None => "none",
                                    Some(ResolvedCellTarget::Path(_)) => "path",
                                    Some(ResolvedCellTarget::Uri(_)) => "uri",
                                    Some(ResolvedCellTarget::Rejected(_)) => "rejected",
                                },
                            },
                            "hwnd": handle.hwnd.get(), "rows": rows, "cw": cell_width, "ch": cell_height,
                            "top": origin.map(|grid_origin| grid_origin[1]), "tab_bar_top": renderer.tab_bar_y_offset(),
                            "surface_height": renderer.height(), "padding_bottom": renderer.padding_bottom_px(),
                            "view_top": GpuRenderer::resolved_view_top_abs_legacy(parser.grid(), pane.viewport_top_abs),
                            "search_current": tab.search.as_ref().and_then(|search| search.current),
                            "search_total": tab.search.as_ref().map(|search| search.matches.len()),
                            "pointer_cell": renderer.pixel_to_pane_cell(window.cursor_pos.0 as f32, window.cursor_pos.1 as f32),
                            "selection_rows": window.selection.as_ref().map(|selection| {let (start,end)=selection.normalized(); [start.0,end.0]}),
                            "padding_left": self.app.config.window.padding_left,
                            "preview": window.link_preview.as_ref().map(|preview| &preview.uri),
                            "notification": window.notification.as_ref().map(|notification| &notification.message),
                            "links": parser.grid().rows_iter().enumerate().flat_map(|(row, cells)| cells.iter().enumerate().filter_map(move |(col, cell)| cell.hyperlink().map(|id| (row,col,id)))).filter_map(|(row,col,id)| parser.hyperlinks().lookup(id).map(|link| serde_json::json!({"row":row,"col":col,"uri":&*link.uri}))).collect::<Vec<_>>()});
                        std::fs::write(
                            self.root.join("window.json"),
                            serde_json::to_vec(&report).unwrap(),
                        )
                        .unwrap();
                    }
                }
            }
            if self.root.join("done").exists() {
                event_loop.exit();
            }
            assert!(
                self.started.elapsed() < std::time::Duration::from_secs(180),
                "native driver deadline"
            );
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(50),
            ));
        }
    }
    assert!(std::env::var_os("NO_COLOR").is_none(), "native color profile required");
    let root = PathBuf::from(
        std::env::var_os("SONICTERM_PATH_INTERACTION_DIR").expect("explicit scratch directory"),
    );
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::create_dir_all(root.join("logs")).unwrap();
    let mut config = Config::default();
    config.terminal.shell = Some("cmd.exe".into());
    if root.join("cold").exists() {
        config.window.warm_window_pool = 0;
    }
    config.window.cols = 110;
    config.window.rows = 28;
    config.logging.level = sonicterm_logging::LogLevel::Debug;
    let _log = sonicterm_logging::init_in(&config.logging, &root.join("logs")).unwrap();
    tracing::warn!(
        target: "sonicterm_app::app::path_target::path_target_tests",
        "native path interaction started"
    );
    let event_loop = EventLoop::<super::super::super::super::UserEvent>::with_user_event()
        .with_any_thread(true)
        .build()
        .unwrap();
    let mut app = App::new_with_proxy(
        Theme::default(),
        config,
        Keymap::parse_resilient(
            &format!(
                "{}\n[[binding]]\nkeys = \"alt+shift+x\"\naction = \"move_tab_to_new_window\"\n",
                include_str!("../../../../../assets/keymaps/sonicterm-windows.toml")
            ),
            "native fixture",
        )
        .unwrap(),
        Some(event_loop.create_proxy()),
    );
    // Inject only fixture path resolution; the child shell must keep the user's real HOME.
    if root.join("home").is_dir() {
        app.home_dir = Some(root.join("home"));
    }
    app.runtime_config_path = Some(root.join("config/sonicterm.toml"));
    let mut probe = NativeProbe {
        app,
        root,
        started: std::time::Instant::now(),
        sequence: 0,
        input_sequence: 0,
        last_pointer_event: serde_json::Value::Null,
    };
    event_loop.run_app(&mut probe).unwrap();
    assert!(
        probe.root.join("done").exists(),
        "driver must verify native outcomes before completion"
    );
}

/// Only an NT drive path `\??\X:\…` or a target relative to the link's folder names a local path;
/// bare and `\\?\` drive paths, UNC, device, volume-GUID, drive-relative and root-relative targets
/// are refused from their text alone, as are `/` separators, `..` after a name, and alternate-stream
/// or reserved names.
#[test]
pub(super) fn link_targets_parse_only_nt_drive_and_relative_forms() {
    fn absolute(drive: char, names: &[&str]) -> Option<LinkTarget> {
        let names = names.iter().map(|name| (*name).to_string()).collect();
        Some(LinkTarget::Absolute { drive, names })
    }
    fn relative(parent_steps: usize, names: &[&str]) -> Option<LinkTarget> {
        let names = names.iter().map(|name| (*name).to_string()).collect();
        Some(LinkTarget::Relative { parent_steps, names })
    }
    let cases = [
        (r"\??\C:\real\notes.txt", absolute('C', &["real", "notes.txt"])),
        (r"\??\c:\real\", absolute('C', &["real"])),
        (r"\??\D:\data", absolute('D', &["data"])),
        (r"\??\C:\", absolute('C', &[])),
        ("notes.txt", relative(0, &["notes.txt"])),
        (r".\sub\notes.txt", relative(0, &["sub", "notes.txt"])),
        (r"..\..\other\", relative(2, &["other"])),
        (r"C:\real\notes.txt", None),
        (r"\\?\C:\real\notes.txt", None),
        (r"\\host\share\notes.txt", None),
        (r"\\?\UNC\host\share\notes.txt", None),
        (r"\??\UNC\host\share\notes.txt", None),
        (r"\\.\pipe\host", None),
        (r"\\?\Volume{00000000-0000-0000-0000-000000000000}\data", None),
        (r"\??\Volume{00000000-0000-0000-0000-000000000000}\data", None),
        (r"\??\GLOBALROOT\Device\Mup\host\share", None),
        (r"\\?\GLOBALROOT\Device\Mup\host\share", None),
        (r"\Device\HarddiskVolume3\data", None),
        (r"\??\C:", None),
        ("C:notes.txt", None),
        (r"\real\notes.txt", None),
        (r"\??\C:/real/notes.txt", None),
        ("sub/notes.txt", None),
        (r"sub\..\notes.txt", None),
        (r"\??\C:\real\..\notes.txt", None),
        (r"\??\C:\real\.\notes.txt", None),
        (r"\??\C:\\notes.txt", None),
        ("notes.txt:stream", None),
        (r"\??\C:\real\notes.txt:stream", None),
        ("notes.", None),
        ("notes ", None),
        ("no*tes", None),
        ("sub\\\u{1}name", None),
        ("", None),
    ];
    for (raw, expected) in cases {
        assert_eq!(parse_link_target(raw), expected, "{raw}");
    }
}

/// Only a local disk may start a walk and only a local fixed disk may hold a followed link or
/// receive its target: a remote disk, a network file system, an optical drive and a virtual disk
/// are refused, and a removable disk only starts a walk.
#[test]
pub(super) fn only_local_fixed_disks_start_walks_or_carry_followed_links() {
    assert!(volume_is_local_disk(FIXED_DISK, false) && volume_is_local_disk(FIXED_DISK, true));
    assert!(volume_is_local_disk(REMOVABLE_DISK, false));
    assert!(!volume_is_local_disk(REMOVABLE_DISK, true));
    for volume in [
        VolumeDevice { device_type: FILE_DEVICE_DISK, characteristics: FILE_REMOTE_DEVICE },
        NETWORK_SHARE,
        VolumeDevice { device_type: FILE_DEVICE_NETWORK_FILE_SYSTEM, characteristics: 0 },
        VolumeDevice { device_type: FILE_DEVICE_CD_ROM, characteristics: FILE_REMOVABLE_MEDIA },
        VolumeDevice { device_type: FILE_DEVICE_VIRTUAL_DISK, characteristics: 0 },
    ] {
        assert!(!volume_is_local_disk(volume, false), "{volume:?}");
        assert!(!volume_is_local_disk(volume, true), "{volume:?}");
    }
}

/// Only an exact `\Device\HarddiskVolume<digits>` target names a drive the walk opens: network
/// redirectors, `Mup`, `subst` folders, optical and RAM drives, shadow copies and near misses are
/// refused.
#[test]
pub(super) fn drive_letters_must_name_exact_hard_disk_volumes() {
    for target in [r"\Device\HarddiskVolume3", r"\Device\HarddiskVolume12"] {
        assert!(is_hard_disk_volume(target), "{target}");
    }
    for target in [
        r"\Device\LanmanRedirector\;Z:0000000000012345\host\share",
        r"\Device\Mup\;LanmanRedirector\;Z:0000000000012345\host\share",
        r"\Device\Mup",
        r"\Device\WebDavRedirector\;Z:0000000000012345\host\share",
        r"\Device\RdpDr\;Z:1\tsclient\C",
        r"\Device\Nfs\;Z:0000000000012345\host\share",
        r"\??\C:\work",
        r"\Device\CdRom0",
        r"\Device\Ramdisk0",
        r"\Device\HarddiskVolumeShadowCopy1",
        r"\Device\HarddiskVolume",
        r"\Device\HarddiskVolume3\",
        r"\Device\HarddiskVolume3\work",
        r"\Device\Harddisk0\Partition1",
        r"\device\harddiskvolume3",
        "",
    ] {
        assert!(!is_hard_disk_volume(target), "{target}");
    }
}

/// A held entry is classified from its own attributes and reparse tag alone: only a symlink or
/// junction tag is a link, with the entry's own folder flag, and any other reparse point is refused.
#[test]
pub(super) fn held_entries_are_classified_from_their_own_attributes_and_tag() {
    let entry =
        |attributes: u32, reparse_tag: u32| entry_kind(EntryAttributes { attributes, reparse_tag });
    assert_eq!(entry(0x10, 0), LocalEntry::Directory);
    assert_eq!(entry(0x20, 0), LocalEntry::File);
    // A tag without the reparse attribute is no reparse point.
    assert_eq!(entry(0x20, IO_REPARSE_TAG_SYMLINK), LocalEntry::File);
    assert_eq!(entry(0x410, IO_REPARSE_TAG_MOUNT_POINT), LocalEntry::Link { directory: true });
    assert_eq!(entry(0x410, IO_REPARSE_TAG_SYMLINK), LocalEntry::Link { directory: true });
    assert_eq!(entry(0x420, IO_REPARSE_TAG_SYMLINK), LocalEntry::Link { directory: false });
    for tag in [IO_REPARSE_TAG_CLOUD, IO_REPARSE_TAG_APPEXECLINK, IO_REPARSE_TAG_LX_SYMLINK, 0] {
        assert_eq!(entry(0x420, tag), LocalEntry::Refused, "{tag:#x}");
        assert_eq!(entry(0x410, tag), LocalEntry::Refused, "{tag:#x}");
    }
}

/// A symlink or junction target is decoded only from a buffer that matches the documented layout:
/// a symlink's relative flag must agree with whether its text starts with `\`, a junction's text
/// must be rooted, and other tags, empty or odd-length names, and truncated buffers give nothing.
#[test]
pub(super) fn reparse_buffers_decode_only_symlink_and_junction_targets() {
    let absolute = reparse_buffer(
        IO_REPARSE_TAG_SYMLINK,
        Some(0),
        r"\??\C:\real\notes.txt",
        r"C:\real\notes.txt",
    );
    assert_eq!(decode_reparse_target(&absolute).as_deref(), Some(r"\??\C:\real\notes.txt"));
    let relative = reparse_buffer(
        IO_REPARSE_TAG_SYMLINK,
        Some(SYMLINK_FLAG_RELATIVE),
        r"..\real\notes.txt",
        r"..\real\notes.txt",
    );
    assert_eq!(decode_reparse_target(&relative).as_deref(), Some(r"..\real\notes.txt"));
    let junction = reparse_buffer(IO_REPARSE_TAG_MOUNT_POINT, None, r"\??\C:\real\", r"C:\real\");
    assert_eq!(decode_reparse_target(&junction).as_deref(), Some(r"\??\C:\real\"));
    let refused = [
        reparse_buffer(IO_REPARSE_TAG_SYMLINK, Some(SYMLINK_FLAG_RELATIVE), r"\??\C:\real", ""),
        reparse_buffer(IO_REPARSE_TAG_SYMLINK, Some(0), r"real\notes.txt", ""),
        reparse_buffer(IO_REPARSE_TAG_SYMLINK, Some(0), "", ""),
        reparse_buffer(IO_REPARSE_TAG_MOUNT_POINT, None, r"real\", ""),
        reparse_buffer(IO_REPARSE_TAG_CLOUD, Some(0), r"\??\C:\real", ""),
    ];
    for buffer in &refused {
        assert_eq!(decode_reparse_target(buffer), None);
    }
    for cut in [0, 3, 7, 11, 19, absolute.len() - 1] {
        assert_eq!(decode_reparse_target(&absolute[..cut]), None, "cut at {cut}");
    }
    let mut odd = absolute.clone();
    // Byte 10 is the low byte of the substitute name's length; one more makes it odd.
    odd[10] += 1;
    assert_eq!(decode_reparse_target(&odd), None);
}

/// Win32 reads `CON`, `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, and `COM` or `LPT` with one digit as
/// devices in any case and with any extension; names that only start like one are ordinary.
#[test]
pub(super) fn dos_device_names_are_recognized_in_any_case_and_with_any_extension() {
    for name in [
        "CON",
        "con",
        "Nul.txt",
        "PRN.tar.gz",
        "aux",
        "COM1",
        "lpt9.log",
        "COM¹",
        "CONIN$",
        "conout$.txt",
        "NUL .txt",
    ] {
        assert!(is_dos_device_name(name), "{name}");
    }
    for name in
        ["CONFIG", "console.txt", "COM10", "LPT0", "COM", "nul-notes.txt", "notes.txt", "LPT1x"]
    {
        assert!(!is_dos_device_name(name), "{name}");
    }
}
