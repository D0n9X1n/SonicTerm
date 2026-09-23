use super::*;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_app::app::{
    os_drag::{AppHandle, TabBarSnapshot},
    UserEvent,
};
use std::path::PathBuf;
use windows::Win32::{
    Foundation::{DRAGDROP_E_ALREADYREGISTERED, DRAGDROP_E_NOTREGISTERED, POINT},
    Graphics::Gdi::ClientToScreen,
    System::SystemServices::MODIFIERKEYS_FLAGS,
    UI::Shell::DROPFILES,
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    platform::windows::{EventLoopBuilderExtWindows, WindowAttributesExtWindows},
    window::Window,
};

type NativeResult<T = ()> = Result<T, String>;

fn require(condition: bool, message: &str) -> NativeResult {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

#[implement(IDataObject)]
struct NativeDataObject {
    formats: Vec<(u16, Vec<u8>)>,
}

impl NativeDataObject {
    fn bytes_for(&self, format: &FORMATETC) -> Option<&[u8]> {
        if format.dwAspect != DVASPECT_CONTENT.0 || format.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
            return None;
        }
        self.formats
            .iter()
            .find(|(id, _)| *id == format.cfFormat)
            .map(|(_, bytes)| bytes.as_slice())
    }
}

#[allow(non_snake_case)]
impl IDataObject_Impl for NativeDataObject_Impl {
    fn GetData(&self, format: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        let format =
            // SAFETY: the COM caller retains this FORMATETC for the synchronous invocation.
            unsafe { &*format };
        let bytes =
            self.bytes_for(format).ok_or_else(|| windows::core::Error::from(DV_E_FORMATETC))?;
        let global =
            // SAFETY: test formats always contain nonempty bytes and use the clipboard's moveable allocation contract.
            unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }?;
        // SAFETY: this allocation belongs to the callback until it is transferred through STGMEDIUM.
        unsafe {
            let destination = GlobalLock(global) as *mut u8;
            if destination.is_null() {
                let _ = GlobalFree(Some(global));
                return Err(E_NOTIMPL.into());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), destination, bytes.len());
            let _ = GlobalUnlock(global);
        }
        let mut medium = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, ..Default::default() };
        medium.u.hGlobal = global;
        Ok(medium)
    }

    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, format: *const FORMATETC) -> HRESULT {
        let format =
            // SAFETY: the COM caller retains this FORMATETC for the synchronous invocation.
            unsafe { &*format };
        if self.bytes_for(format).is_some() {
            S_OK
        } else {
            DV_E_FORMATETC
        }
    }

    fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, _: *mut FORMATETC) -> HRESULT {
        DV_E_FORMATETC
    }

    fn SetData(
        &self,
        _: *const FORMATETC,
        _: *const STGMEDIUM,
        _: BOOL,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, _: u32) -> windows::core::Result<IEnumFORMATETC> {
        Err(E_NOTIMPL.into())
    }

    fn DAdvise(
        &self,
        _: *const FORMATETC,
        _: u32,
        _: windows::core::Ref<windows::Win32::System::Com::IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _: u32) -> windows::core::Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> windows::core::Result<windows::Win32::System::Com::IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

fn file_data(paths: &[&str]) -> IDataObject {
    let mut bytes = vec![0; std::mem::size_of::<DROPFILES>()];
    let offset = std::mem::offset_of!(DROPFILES, pFiles);
    bytes[offset..offset + 4]
        .copy_from_slice(&(std::mem::size_of::<DROPFILES>() as u32).to_le_bytes());
    let wide = std::mem::offset_of!(DROPFILES, fWide);
    bytes[wide..wide + 4].copy_from_slice(&1_u32.to_le_bytes());
    for path in paths {
        for unit in path.encode_utf16().chain(std::iter::once(0)) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
    }
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    NativeDataObject { formats: vec![(CF_HDROP.0, bytes)] }.into()
}

fn hwnd(window: &Window) -> NativeResult<HWND> {
    let handle = window.window_handle().map_err(|error| error.to_string())?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("native drop test did not receive an HWND".to_owned());
    };
    Ok(HWND(handle.hwnd.get() as *mut _))
}

fn client_point(window: &Window) -> NativeResult<POINTL> {
    let mut point = POINT { x: 10, y: 10 };
    let converted =
        // SAFETY: window retains its HWND on this thread and ClientToScreen borrows the stack POINT synchronously.
        unsafe { ClientToScreen(hwnd(window)?, &mut point) }.as_bool();
    require(converted, "ClientToScreen failed")?;
    Ok(POINTL { x: point.x, y: point.y })
}

fn invoke_drop(
    target: &IDropTarget,
    data: &IDataObject,
    point: POINTL,
) -> NativeResult<DROPEFFECT> {
    let mut effect = DROPEFFECT_COPY | DROPEFFECT_MOVE;
    // SAFETY: all interfaces, the point value and effect storage remain live on their OLE-initialized thread.
    unsafe { target.Drop(data, MODIFIERKEYS_FLAGS(0), point, &mut effect) }
        .map_err(|error| error.to_string())?;
    Ok(effect)
}

fn publish_bar(handle: &AppHandle, window: Option<WindowId>, point: &POINTL, slot: usize) {
    handle.tab_bar_registry().publish(TabBarSnapshot {
        window,
        window_rect: (point.x - 5, point.y - 5, point.x + 50, point.y + 50),
        bar_rect: (point.x - 5, point.y - 5, point.x + 50, point.y + 20),
        tab_midpoints: vec![point.x + 20],
        tab_indices: vec![slot],
        total_tabs: slot + 1,
        overflow_append_from: None,
    });
}

struct DropStateGuard;

impl Drop for DropStateGuard {
    fn drop(&mut self) {
        clear_drop_outcome_handle();
        let _ = sonicterm_app::os_drag_bridge::__test_drain_files();
    }
}

fn exercise_native_contract(
    el: &ActiveEventLoop,
    proxy: &EventLoopProxy<UserEvent>,
) -> NativeResult {
    let uninitialized = el
        .create_window(Window::default_attributes().with_visible(false).with_drag_and_drop(false))
        .map_err(|error| error.to_string())?;
    let missing_ole =
        // SAFETY: uninitialized owns this thread's live HWND; absent OLE must be refused without acquiring native custody.
        unsafe { register_for_window(hwnd(&uninitialized)?, uninitialized.id()) };
    require(
        missing_ole.as_ref().is_err_and(|error| error.code() == CO_E_NOTINITIALIZED),
        "registration without OleGuard was not refused",
    )?;
    drop(uninitialized);
    let ole = init_ole().ok_or("native drop test could not initialize OLE")?;
    let result = (|| {
        let _drop_state = DropStateGuard;
        sonicterm_app::os_drag_bridge::install_proxy(proxy.clone());
        let make_window = |default_owner| {
            el.create_window(
                Window::default_attributes()
                    .with_visible(false)
                    .with_inner_size(winit::dpi::PhysicalSize::new(200, 120))
                    .with_drag_and_drop(default_owner),
            )
            .map(Arc::new)
            .map_err(|error| error.to_string())
        };
        let default_window = make_window(true)?;
        let main = make_window(false)?;
        let child = make_window(false)?;
        let fresh = make_window(false)?;
        let handle = AppHandle::new(proxy.clone()).with_main_window(Some(main.id()));
        let mut refused_backend =
            // SAFETY: ole retains this thread's OLE initialization until all backends and windows in this closure are dropped.
            unsafe { crate::tab_drag_os::WinOsTabDragBackend::boxed() };
        require(
            refused_backend
                .register_window(handle.clone(), default_window.id(), &default_window)
                .is_err(),
            "custom backend replaced winit's default owner",
        )?;
        let duplicate =
            // SAFETY: default_window retains the same HWND and its default target; this duplicate must not revoke it.
            unsafe { register_for_window(hwnd(&default_window)?, default_window.id()) };
        require(
            duplicate.as_ref().is_err_and(|error| error.code() == DRAGDROP_E_ALREADYREGISTERED),
            "refused registration disturbed the default owner",
        )?;
        drop(refused_backend);
        let (mut backend, report) =
            // SAFETY: ole encloses every registration and this backend's eventual Drop.
            unsafe { crate::tab_drag_os::WinOsTabDragBackend::boxed_for_smoke() };
        require(
            backend.owns_native_drop_target(),
            "Windows custom backend did not claim native ownership",
        )?;
        for window in [&main, &child, &fresh] {
            backend.register_window(handle.clone(), window.id(), window)?;
        }
        backend.register_window(handle.clone(), main.id(), &main)?;
        let duplicate =
            // SAFETY: main retains its registered HWND; direct duplicate registration must fail without replacing its target.
            unsafe { register_for_window(hwnd(&main)?, main.id()) };
        require(
            duplicate.as_ref().is_err_and(|error| error.code() == DRAGDROP_E_ALREADYREGISTERED),
            "duplicate custom registration was not refused",
        )?;
        let main_target: IDropTarget = DropTarget::new(hwnd(&main)?, main.id()).into();
        let child_target: IDropTarget = DropTarget::new(hwnd(&child)?, child.id()).into();
        let main_point = client_point(&main)?;
        let child_point = client_point(&child)?;
        let paths = [r"C:\folder\空白 file.txt", r"D:\日本語\二.txt"];
        let files = file_data(&paths);
        let mut entered = DROPEFFECT_COPY | DROPEFFECT_MOVE;
        // SAFETY: the production target and real IDataObject remain live for this synchronous COM callback.
        unsafe { child_target.DragEnter(&files, MODIFIERKEYS_FLAGS(0), child_point, &mut entered) }
            .map_err(|error| error.to_string())?;
        require(entered == DROPEFFECT_COPY, "file DragEnter did not offer COPY")?;
        require(
            invoke_drop(&child_target, &files, child_point)? == DROPEFFECT_COPY,
            "Unicode file drop was not admitted",
        )?;
        require(
            invoke_drop(&main_target, &files, main_point)? == DROPEFFECT_COPY,
            "main file drop was not admitted",
        )?;
        let expected: Vec<PathBuf> = paths.iter().map(|path| PathBuf::from(*path)).collect();
        require(
            sonicterm_app::os_drag_bridge::__test_drain_files()
                == vec![(child.id(), expected.clone()), (main.id(), expected)],
            "file destination or UTF-16 decoding changed in the bridge",
        )?;
        let payload = TabPayload {
            pty_pid: 1,
            tab_title: "test".into(),
            scrollback_b64: String::new(),
            cwd: String::new(),
            cmd: "cmd.exe".into(),
            env: Vec::new(),
        }
        .to_json()
        .map_err(|error| error.to_string())?;
        let data: IDataObject = SonicTermDataObject { json: payload.as_bytes().to_vec() }.into();
        let mut foreign_effect = DROPEFFECT_COPY | DROPEFFECT_MOVE;
        // SAFETY: production DragEnter receives live COM interfaces and stack output on their owning thread.
        unsafe {
            main_target.DragEnter(&data, MODIFIERKEYS_FLAGS(0), main_point, &mut foreign_effect)
        }
        .map_err(|error| error.to_string())?;
        require(
            foreign_effect == DROPEFFECT_NONE,
            "foreign tab advertised acceptance before drop",
        )?;
        foreign_effect = DROPEFFECT_COPY | DROPEFFECT_MOVE;
        // SAFETY: DragOver retains the target and a live effect output throughout the synchronous invocation.
        unsafe { main_target.DragOver(MODIFIERKEYS_FLAGS(0), main_point, &mut foreign_effect) }
            .map_err(|error| error.to_string())?;
        require(
            foreign_effect == DROPEFFECT_NONE,
            "foreign tab regained an unadmitted DragOver effect",
        )?;
        require(
            invoke_drop(&main_target, &data, main_point)? == DROPEFFECT_NONE,
            "foreign tab was acknowledged without a local transfer",
        )?;
        install_drop_outcome_handle(handle.clone(), &payload);
        let malformed: IDataObject = SonicTermDataObject { json: b"{".to_vec() }.into();
        require(
            invoke_drop(&main_target, &malformed, main_point)? == DROPEFFECT_NONE,
            "malformed tab was acknowledged",
        )?;
        require(
            handle.pending_handle().take_ended() == Some(DragOutcome::Cancelled),
            "malformed tab did not preserve the active source",
        )?;
        let different: IDataObject =
            SonicTermDataObject { json: payload.replace("test", "foreign").into_bytes() }.into();
        require(
            invoke_drop(&main_target, &different, main_point)? == DROPEFFECT_NONE,
            "foreign payload borrowed a live gesture",
        )?;
        handle.pending_handle().take_ended();
        publish_bar(&handle, Some(child.id()), &main_point, 4);
        publish_bar(&handle, None, &main_point, 1);
        require(
            invoke_drop(&main_target, &data, main_point)? == DROPEFFECT_MOVE,
            "same-process main target refused its matching payload",
        )?;
        require(
            handle.pending_handle().take_ended()
                == Some(DragOutcome::DroppedOnBar { target_window: None, target_slot: 1 }),
            "overlapping child snapshot stole the main drop",
        )?;
        publish_bar(&handle, None, &child_point, 1);
        publish_bar(&handle, Some(child.id()), &child_point, 4);
        require(
            invoke_drop(&child_target, &data, child_point)? == DROPEFFECT_MOVE,
            "same-process child target refused its matching payload",
        )?;
        require(
            handle.pending_handle().take_ended()
                == Some(DragOutcome::DroppedOnBar {
                    target_window: Some(child.id()),
                    target_slot: 4,
                }),
            "overlapping main snapshot stole the child drop",
        )?;
        handle.tab_bar_registry().remove(Some(child.id()));
        require(
            invoke_drop(&child_target, &data, child_point)? == DROPEFFECT_MOVE,
            "valid child client drop was refused",
        )?;
        require(
            handle.pending_handle().take_ended()
                == Some(DragOutcome::DroppedOnEmpty {
                    drop_screen_pos: (child_point.x, child_point.y),
                }),
            "client drop borrowed another window's bar",
        )?;
        require(
            invoke_drop(&child_target, &data, POINTL { x: i32::MIN, y: i32::MIN })?
                == DROPEFFECT_NONE,
            "out-of-window drop was acknowledged",
        )?;
        require(
            handle.pending_handle().take_ended() == Some(DragOutcome::Cancelled),
            "invalid native destination did not cancel",
        )?;
        clear_drop_outcome_handle();
        backend.unregister_window(child.id())?;
        backend.unregister_window(child.id())?;
        backend.unregister_window(fresh.id())?;
        let retained_main = Arc::downgrade(&main);
        drop(main);
        let main = retained_main.upgrade().ok_or("backend did not retain its registered window")?;
        drop(backend);
        report.lock().unwrap_or_else(|error| error.into_inner()).validate()?;
        let revoked =
            // SAFETY: main remains live after backend teardown, and the already-revoked HWND must now report no registration.
            unsafe { unregister_for_window(hwnd(&main)?) };
        require(
            revoked.as_ref().is_err_and(|error| error.code() == DRAGDROP_E_NOTREGISTERED),
            "backend Drop left the main target registered",
        )?;
        let mut failed_cleanup =
            // SAFETY: this second backend is retained inside the same OleGuard scope for failed-revocation custody testing.
            unsafe { crate::tab_drag_os::WinOsTabDragBackend::boxed() };
        failed_cleanup.register_window(handle, fresh.id(), &fresh)?;
        // SAFETY: fresh pins its HWND; deliberately remove the registration to exercise the backend's failure path.
        unsafe { unregister_for_window(hwnd(&fresh)?) }.map_err(|error| error.to_string())?;
        require(
            failed_cleanup.unregister_window(fresh.id()).is_err(),
            "failed native revocation was reported as successful",
        )?;
        let retained_fresh = Arc::downgrade(&fresh);
        drop(fresh);
        require(retained_fresh.upgrade().is_some(), "failed revocation discarded window custody")?;
        drop(failed_cleanup);
        require(
            retained_fresh.upgrade().is_none(),
            "final backend teardown retained its window indefinitely",
        )?;
        Ok(())
    })();
    drop(ole);
    result
}

#[test]
fn native_drop_ownership_and_com_delivery_preserve_window_identity() {
    // Real hidden HWNDs and COM media exercise ownership and decoding without synthesizing mouse or keyboard events.
    struct Probe {
        proxy: EventLoopProxy<UserEvent>,
        result: Option<NativeResult>,
    }
    impl ApplicationHandler<UserEvent> for Probe {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            self.result = Some(exercise_native_contract(el, &self.proxy));
            el.exit();
        }
        fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
            if event == UserEvent::RuntimeSmokeTimeout {
                self.result = Some(Err("native drop test exceeded its startup deadline".into()));
                el.exit();
            }
        }
        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .with_any_thread(true)
        .build()
        .expect("native event loop");
    let proxy = event_loop.create_proxy();
    let watchdog_proxy = proxy.clone();
    let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
    let watchdog = std::thread::spawn(move || {
        if matches!(
            cancelled.recv_timeout(std::time::Duration::from_secs(30)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ) {
            let _ = watchdog_proxy.send_event(UserEvent::RuntimeSmokeTimeout);
        }
    });
    let mut probe = Probe { proxy, result: None };
    let result = event_loop.run_app(&mut probe);
    let _ = cancel.send(());
    watchdog.join().expect("native test watchdog");
    result.expect("native event loop run");
    assert_eq!(probe.result, Some(Ok(())));
}
