//! The Linux OSC 52 read: an X11 client that asks the selection owner for `UTF8_STRING` and checks
//! every reply with `osc52_read_x11` before keeping it. Wayland sessions read through XWayland;
//! with no X server the read is `Unsupported` and fails closed, as paste does.

use std::time::Instant;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    self, Atom, ConnectionExt as _, CreateWindowAux, EventMask, Window, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use super::osc52_read_source::{ReadLocation, ReadOutcome};
use super::osc52_read_x11::{
    classify_first, fetch_length_units, FirstReply, IncrTransfer, PropertyKind, PropertyReply,
    SELECTION_WAIT,
};

/// How long the client sleeps between polls while it waits for an event.
const POLL_PAUSE: std::time::Duration = std::time::Duration::from_millis(5);

/// The atoms a read compares replies with.
struct Atoms {
    utf8: Atom,
    string: Atom,
    text_plain: Atom,
    incr: Atom,
    property: Atom,
}

/// Read `location` within `max_bytes`; any protocol error fails the read.
pub(in crate::app) fn read(location: ReadLocation, max_bytes: usize) -> ReadOutcome {
    let Ok((connection, screen)) = RustConnection::connect(None) else {
        // When: connect fails, no X server is reachable.
        return ReadOutcome::Unsupported;
    };
    read_with(&connection, screen, location, max_bytes).unwrap_or(ReadOutcome::Failed)
}

/// Intern `name`.
fn atom(connection: &RustConnection, name: &[u8]) -> Option<Atom> {
    Some(connection.intern_atom(false, name).ok()?.reply().ok()?.atom)
}

/// Run one read on `connection`; `None` on a protocol error.
fn read_with(
    connection: &RustConnection,
    screen: usize,
    location: ReadLocation,
    max_bytes: usize,
) -> Option<ReadOutcome> {
    let root = connection.setup().roots.get(screen)?.root;
    let window = connection.generate_id().ok()?;
    let aux = CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE);
    connection
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &aux,
        )
        .ok()?;
    let selection = match location {
        ReadLocation::Clipboard => atom(connection, b"CLIPBOARD")?,
        ReadLocation::Primary => xproto::AtomEnum::PRIMARY.into(),
    };
    let atoms = Atoms {
        utf8: atom(connection, b"UTF8_STRING")?,
        string: xproto::AtomEnum::STRING.into(),
        text_plain: atom(connection, b"text/plain;charset=utf-8")?,
        incr: atom(connection, b"INCR")?,
        property: atom(connection, b"SONICTERM_OSC52_READ")?,
    };
    connection
        .convert_selection(window, selection, atoms.utf8, atoms.property, x11rb::CURRENT_TIME)
        .ok()?;
    connection.flush().ok()?;
    let notify = wait_for(connection, Instant::now() + SELECTION_WAIT, |event| match event {
        Event::SelectionNotify(notify) if notify.requestor == window => Some(notify.property),
        _ => None,
    })?;
    if notify == x11rb::NONE {
        // When: notify is NONE, the owner answered with no property: nothing it can convert.
        return Some(ReadOutcome::Empty);
    }
    let first = fetch(connection, window, &atoms, max_bytes)?;
    let started = Instant::now();
    if let FirstReply::Done(outcome) = classify_first(&first, max_bytes) {
        // When: classify_first answers at once, the fetch already deleted the property.
        return Some(outcome);
    }
    // Deleting the INCR property (the fetch did) asks the owner for the first chunk.
    let mut transfer = IncrTransfer::new(max_bytes, started);
    loop {
        let deadline = transfer.chunk_deadline(Instant::now());
        wait_for(connection, deadline, |event| match event {
            Event::PropertyNotify(change)
                if change.window == window
                    && change.atom == atoms.property
                    && change.state == xproto::Property::NEW_VALUE =>
            {
                Some(())
            }
            _ => None,
        })?;
        let chunk = fetch(connection, window, &atoms, transfer.remaining())?;
        if let Some(outcome) = transfer.push(chunk) {
            // When: push ends the transfer, a property left by a refused chunk is removed.
            let _ = connection.delete_property(window, atoms.property);
            let _ = connection.flush();
            return Some(outcome);
        }
    }
}

/// Fetch and delete the read property, asking for one 32-bit unit past `max_bytes`.
fn fetch(
    connection: &RustConnection,
    window: Window,
    atoms: &Atoms,
    max_bytes: usize,
) -> Option<PropertyReply> {
    let reply = connection
        .get_property(
            true,
            window,
            atoms.property,
            xproto::AtomEnum::ANY,
            0,
            fetch_length_units(max_bytes),
        )
        .ok()?
        .reply()
        .ok()?;
    let kind = match reply.type_ {
        kind if kind == atoms.utf8 || kind == atoms.text_plain => PropertyKind::Utf8,
        kind if kind == atoms.string => PropertyKind::Latin1,
        kind if kind == atoms.incr => PropertyKind::Incr,
        _ => PropertyKind::Other,
    };
    Some(PropertyReply {
        kind,
        format: reply.format,
        bytes_after: reply.bytes_after,
        value: reply.value,
    })
}

/// Wait until `deadline` for an event `select` accepts; `None` on timeout or a connection error.
fn wait_for<T>(
    connection: &RustConnection,
    deadline: Instant,
    mut select: impl FnMut(&Event) -> Option<T>,
) -> Option<T> {
    loop {
        while let Some(event) = connection.poll_for_event().ok()? {
            if let Some(found) = select(&event) {
                // When: select accepts the event, the wait ends.
                return Some(found);
            }
        }
        if Instant::now() >= deadline {
            // When: the deadline passed, the owner did not answer in time.
            return None;
        }
        std::thread::sleep(POLL_PAUSE);
    }
}
