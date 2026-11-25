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

use super::osc52_read_source::{
    list_native_types, ReadLocation, ReadOutcome, TypeListing, MAX_NATIVE_TYPES_SCANNED,
};
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

/// Create the input-only window a conversion is delivered to, and name the selection `location`
/// reads; `None` on a protocol error.
fn requestor(
    connection: &RustConnection,
    screen: usize,
    location: ReadLocation,
) -> Option<(Window, Atom)> {
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
    Some((window, selection))
}

/// The selection owner's `TARGETS` at `location` as an OSC 5522 listing: the conversion carries
/// atoms only, never content. No X server reads as unsupported; any protocol error fails.
pub(in crate::app) fn list_targets(location: ReadLocation) -> TypeListing {
    let Ok((connection, screen)) = RustConnection::connect(None) else {
        // When: connect fails, no X server is reachable.
        return TypeListing::Unsupported;
    };
    targets_with(&connection, screen, location).map_or(TypeListing::Failed, list_native_types)
}

/// The offer `location` holds now: the selection owner's window in the high 32 bits and its
/// `TIMESTAMP` answer (when it took the selection) in the low 32, so a new owner or a new offer
/// changes it. `None` with no X server, no owner, a protocol error, a missed 2 s selection wait
/// or an owner that cannot answer `TIMESTAMP`, so no paste grant binds to it.
pub(in crate::app) fn offer_version(location: ReadLocation) -> Option<u64> {
    let (connection, screen) = RustConnection::connect(None).ok()?;
    let (window, selection) = requestor(&connection, screen, location)?;
    let owner = connection.get_selection_owner(selection).ok()?.reply().ok()?.owner;
    if owner == x11rb::NONE {
        // When: owner is NONE, nothing is offered, so there is no version.
        return None;
    }
    let timestamp = atom(&connection, b"TIMESTAMP")?;
    let property = atom(&connection, b"SONICTERM_OSC5522_TIMESTAMP")?;
    connection
        .convert_selection(window, selection, timestamp, property, x11rb::CURRENT_TIME)
        .ok()?;
    connection.flush().ok()?;
    let notify = wait_for(&connection, Instant::now() + SELECTION_WAIT, |event| match event {
        Event::SelectionNotify(notify) if notify.requestor == window => Some(notify.property),
        _ => None,
    })?;
    if notify == x11rb::NONE {
        // When: notify is NONE, the owner cannot answer TIMESTAMP.
        return None;
    }
    let reply = connection
        .get_property(true, window, property, xproto::AtomEnum::ANY, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    let stamp = reply.value32()?.next()?;
    Some((u64::from(owner) << 32) | u64::from(stamp))
}

/// Ask the owner for `TARGETS` within the selection wait and name at most
/// [`MAX_NATIVE_TYPES_SCANNED`] of its atoms; `None` on a protocol error, a missed deadline or a
/// reply that is not an atom list.
fn targets_with(
    connection: &RustConnection,
    screen: usize,
    location: ReadLocation,
) -> Option<Vec<String>> {
    let (window, selection) = requestor(connection, screen, location)?;
    let targets = atom(connection, b"TARGETS")?;
    let property = atom(connection, b"SONICTERM_OSC5522_TARGETS")?;
    connection.convert_selection(window, selection, targets, property, x11rb::CURRENT_TIME).ok()?;
    connection.flush().ok()?;
    let notify = wait_for(connection, Instant::now() + SELECTION_WAIT, |event| match event {
        Event::SelectionNotify(notify) if notify.requestor == window => Some(notify.property),
        _ => None,
    })?;
    if notify == x11rb::NONE {
        // When: notify is NONE, the owner offers no targets.
        return Some(Vec::new());
    }
    let units = u32::try_from(MAX_NATIVE_TYPES_SCANNED).unwrap_or(u32::MAX);
    let reply = connection
        .get_property(true, window, property, xproto::AtomEnum::ANY, 0, units)
        .ok()?
        .reply()
        .ok()?;
    let atom_list = reply.type_ == Atom::from(xproto::AtomEnum::ATOM) || reply.type_ == targets;
    if !atom_list {
        // When: atom_list is false (an INCR transfer or another type), the listing fails.
        return None;
    }
    let listed: Vec<Atom> = reply.value32()?.take(MAX_NATIVE_TYPES_SCANNED).collect();
    // Every name request is sent before any reply is read, so one round trip names them all.
    let cookies: Vec<_> =
        listed.iter().filter_map(|target| connection.get_atom_name(*target).ok()).collect();
    let names = cookies.into_iter().filter_map(|cookie| cookie.reply().ok());
    Some(names.map(|reply| String::from_utf8_lossy(&reply.name).into_owned()).collect())
}

/// Run one read on `connection`; `None` on a protocol error.
fn read_with(
    connection: &RustConnection,
    screen: usize,
    location: ReadLocation,
    max_bytes: usize,
) -> Option<ReadOutcome> {
    let (window, selection) = requestor(connection, screen, location)?;
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
    if let FirstReply::Done(outcome) = classify_first(first, max_bytes) {
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
