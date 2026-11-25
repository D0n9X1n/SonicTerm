//! The Linux OSC 52 read: an X11 client implementing `osc52_read_x11::SelectionPort` on its own
//! connection and requestor window, so the portable protocol checks every reply and every deadline
//! before anything is kept. Wayland sessions read through XWayland; with no X server the read is
//! `Unsupported` and fails closed, as paste does.

use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    self, Atom, ConnectionExt as _, CreateWindowAux, EventMask, Window, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use super::osc52_read_source::{
    ReadLocation, ReadOutcome, TypeListing, TypeRead, MAX_NATIVE_TYPES_SCANNED,
};
use super::osc52_read_x11::{
    fetch_length_units, offer_version_with, read_selection, wait_for_notify, ConvertTarget, Polled,
    PropertyKind, PropertyReply, SelectionPort, SELECTION_WAIT,
};

/// How long the client sleeps between polls while it waits for an event.
const POLL_PAUSE: Duration = Duration::from_millis(5);

/// Why a port could not be opened.
enum OpenError {
    /// No X server is reachable.
    NoServer,
    /// The server refused a request.
    Protocol,
}

/// One X11 read: a connection, the input-only requestor window conversions are delivered to, the
/// selection it reads, the property every conversion lands in, and the atoms replies are compared
/// with. Dropping it closes the connection, which destroys the window.
struct NativePort {
    connection: RustConnection,
    window: Window,
    selection: Atom,
    property: Atom,
    requested: Option<Atom>,
    utf8: Atom,
    text_plain: Atom,
    incr: Atom,
    timestamp: Atom,
    targets: Atom,
}

impl NativePort {
    /// Connect and set up a read of `location` into the property named `property_name`.
    fn open(location: ReadLocation, property_name: &[u8]) -> Result<Self, OpenError> {
        let (connection, screen) =
            RustConnection::connect(None).map_err(|_| OpenError::NoServer)?;
        Self::on(connection, screen, location, property_name).ok_or(OpenError::Protocol)
    }

    /// Create the requestor window on `connection` and intern every atom in one round trip.
    fn on(
        connection: RustConnection,
        screen: usize,
        location: ReadLocation,
        property_name: &[u8],
    ) -> Option<Self> {
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
        let names: [&[u8]; 7] = [
            b"CLIPBOARD",
            b"UTF8_STRING",
            b"text/plain;charset=utf-8",
            b"INCR",
            b"TIMESTAMP",
            b"TARGETS",
            property_name,
        ];
        // Every intern request is sent before any reply is read, so one round trip names them all.
        let cookies = names
            .iter()
            .map(|name| connection.intern_atom(false, name))
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        let atoms = cookies
            .into_iter()
            .map(|cookie| cookie.reply().map(|reply| reply.atom))
            .collect::<Result<Vec<Atom>, _>>()
            .ok()?;
        let [clipboard, utf8, text_plain, incr, timestamp, targets, property] =
            <[Atom; 7]>::try_from(atoms).ok()?;
        let selection = match location {
            ReadLocation::Clipboard => clipboard,
            ReadLocation::Primary => xproto::AtomEnum::PRIMARY.into(),
        };
        Some(Self {
            connection,
            window,
            selection,
            property,
            requested: None,
            utf8,
            text_plain,
            incr,
            timestamp,
            targets,
        })
    }

    /// Ask the owner to convert the selection to `target` into the read property.
    fn convert_to(&mut self, target: Atom) -> bool {
        let sent = self.connection.convert_selection(
            self.window,
            self.selection,
            target,
            self.property,
            x11rb::CURRENT_TIME,
        );
        sent.is_ok() && self.connection.flush().is_ok()
    }

    /// The owner's `TARGETS` within the selection wait, at most [`MAX_NATIVE_TYPES_SCANNED`] atoms
    /// named; `None` on a protocol error, a missed deadline or a reply that is not an atom list.
    fn targets(&mut self) -> Option<Vec<String>> {
        let targets = self.targets;
        if !self.convert_to(targets) {
            // When: convert_to fails, the request never reached the server.
            return None;
        }
        let deadline = Instant::now() + SELECTION_WAIT;
        if !wait_for_notify(self, deadline)? {
            // When: wait_for_notify reports no stored property, the owner offers no targets.
            return Some(Vec::new());
        }
        let units = u32::try_from(MAX_NATIVE_TYPES_SCANNED).unwrap_or(u32::MAX);
        let reply = self
            .connection
            .get_property(true, self.window, self.property, xproto::AtomEnum::ANY, 0, units)
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
        let cookies: Vec<_> = listed
            .iter()
            .filter_map(|target| self.connection.get_atom_name(*target).ok())
            .collect();
        let names = cookies.into_iter().filter_map(|cookie| cookie.reply().ok());
        Some(names.map(|reply| String::from_utf8_lossy(&reply.name).into_owned()).collect())
    }
}

impl SelectionPort for NativePort {
    fn owner(&mut self) -> Option<u32> {
        let reply = self.connection.get_selection_owner(self.selection).ok()?.reply().ok()?;
        Some(reply.owner)
    }

    fn convert(&mut self, target: ConvertTarget) -> bool {
        let target = match target {
            ConvertTarget::Text => self.utf8,
            ConvertTarget::Timestamp => self.timestamp,
            ConvertTarget::Mime(atom) => {
                self.requested = Some(atom);
                atom
            }
        };
        self.convert_to(target)
    }

    fn poll(&mut self) -> Polled {
        match self.connection.poll_for_event() {
            Err(_) => Polled::Closed,
            Ok(None) => Polled::Idle,
            Ok(Some(Event::SelectionNotify(notify))) if notify.requestor == self.window => {
                Polled::Notify(notify.property != x11rb::NONE)
            }
            Ok(Some(Event::PropertyNotify(change)))
                if change.window == self.window
                    && change.atom == self.property
                    && change.state == xproto::Property::NEW_VALUE =>
            {
                Polled::NewValue
            }
            Ok(Some(_)) => Polled::Other,
        }
    }

    fn fetch(&mut self, delete: bool, max_bytes: usize) -> Option<PropertyReply> {
        let reply = self
            .connection
            .get_property(
                delete,
                self.window,
                self.property,
                xproto::AtomEnum::ANY,
                0,
                fetch_length_units(max_bytes),
            )
            .ok()?
            .reply()
            .ok()?;
        let kind = match reply.type_ {
            kind if Some(kind) == self.requested => PropertyKind::Requested,
            kind if kind == self.utf8 || kind == self.text_plain => PropertyKind::Utf8,
            kind if kind == Atom::from(xproto::AtomEnum::STRING) => PropertyKind::Latin1,
            kind if kind == self.incr => PropertyKind::Incr,
            _ => PropertyKind::Other,
        };
        // The reply's value moves into the result; nothing is copied.
        Some(PropertyReply {
            kind,
            format: reply.format,
            bytes_after: reply.bytes_after,
            value: reply.value,
        })
    }

    fn delete(&mut self) {
        let _ = self.connection.delete_property(self.window, self.property);
        let _ = self.connection.flush();
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn pause(&mut self) {
        std::thread::sleep(POLL_PAUSE);
    }
}

/// Read `location` within `max_bytes`; any protocol error fails the read.
pub(in crate::app) fn read(location: ReadLocation, max_bytes: usize) -> ReadOutcome {
    match NativePort::open(location, b"SONICTERM_OSC52_READ") {
        Ok(mut port) => read_selection(&mut port, max_bytes).unwrap_or(ReadOutcome::Failed),
        Err(OpenError::NoServer) => ReadOutcome::Unsupported,
        Err(OpenError::Protocol) => ReadOutcome::Failed,
    }
}

/// The selection owner's `TARGETS` at `location` as an OSC 5522 listing: the conversion carries
/// atoms only, never content. No X server reads as unsupported; any protocol error fails.
pub(in crate::app) fn list_targets(location: ReadLocation) -> TypeListing {
    match NativePort::open(location, b"SONICTERM_OSC5522_TARGETS") {
        Ok(mut port) => {
            port.targets().map_or(TypeListing::Failed, super::osc52_read_x11::list_mime_atoms)
        }
        Err(OpenError::NoServer) => TypeListing::Unsupported,
        Err(OpenError::Protocol) => TypeListing::Failed,
    }
}

/// The offer `location` holds now, as `osc52_read_x11::offer_version_with` reads it; `None` with
/// no X server or on any protocol error, so no paste grant binds to it.
pub(in crate::app) fn offer_version(location: ReadLocation) -> Option<u64> {
    let mut port = NativePort::open(location, b"SONICTERM_OSC5522_TIMESTAMP").ok()?;
    offer_version_with(&mut port)
}

/// Read the bytes under an existing MIME atom. A read must never create a permanent atom.
pub(in crate::app) fn read_type(location: ReadLocation, mime: &str, max_bytes: usize) -> TypeRead {
    if !super::kitty_clipboard_offer::valid_mime(mime) {
        // When: valid_mime rejects mime, never request that atom from an external owner.
        return TypeRead::Absent;
    }
    let mut port = match NativePort::open(location, b"SONICTERM_OSC5522_READ") {
        Ok(port) => port,
        Err(OpenError::NoServer) => {
            // When: NoServer, a native-only Wayland session has no backend for this read.
            return TypeRead::Unsupported;
        }
        Err(OpenError::Protocol) => {
            // When: Protocol setup failed on an X server, this is a native I/O failure.
            return TypeRead::Failed;
        }
    };
    let Some(atom) = port
        .connection
        .intern_atom(true, mime.as_bytes())
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| reply.atom)
    else {
        // When: intern_atom failed, the server could not resolve the requested MIME.
        return TypeRead::Failed;
    };
    if atom == x11rb::NONE {
        // When: atom is NONE, the name does not exist; lookup must not create it.
        return TypeRead::Absent;
    }
    super::osc52_read_x11::read_mime(&mut port, atom, max_bytes).unwrap_or(TypeRead::Failed)
}
