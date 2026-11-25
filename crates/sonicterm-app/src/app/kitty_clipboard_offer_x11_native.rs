//! One X11 event thread multiplexes the command socket, both selections and their draining
//! offers. Each offer has a distinct owner window, so a delayed SelectionClear cannot retire its
//! replacement. Destroying that window releases only selections still owned by that offer.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, SyncSender};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::{
    self, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask,
    PropMode, SelectionNotifyEvent, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::*;
use crate::app::osc52_read_ticket::WriteBinding;

struct Command {
    offer: NativeOffer,
    lease: ClipLease,
    targets: ClipTargets,
    binding: WriteBinding,
    answer: SyncSender<WriteResult>,
}

/// FIFO handoff from the clipboard worker. The worker waits for confirmation before taking the
/// next job; dropping the socket wakes the server to release its windows and leases.
#[derive(Default)]
pub(in crate::app) struct SelectionServer {
    channel: Option<(SyncSender<Command>, UnixStream)>,
}

impl SelectionServer {
    /// Stage and confirm one complete offer before the ordering domain advances.
    pub(in crate::app) fn publish(
        &mut self,
        offer: NativeOffer,
        lease: ClipLease,
        targets: ClipTargets,
        binding: WriteBinding,
    ) -> WriteResult {
        if self.channel.is_none() {
            self.channel = Self::start();
        }
        let Some((commands, wake)) = self.channel.as_mut() else {
            // When: channel could not start, no X selection was touched.
            return WriteResult::each(targets, TargetOutcome::Refused);
        };
        let (answer, result) = mpsc::sync_channel(1);
        let command = Command { offer, lease, targets, binding, answer };
        if commands.send(command).is_err() || wake.write_all(&[1]).is_err() {
            // When: commands or wake disconnected, drop the socket so the next job can reconnect.
            self.channel = None;
            return WriteResult::each(targets, TargetOutcome::Refused);
        }
        match result.recv() {
            Ok(published) => published,
            Err(_) => {
                self.channel = None;
                WriteResult::each(targets, TargetOutcome::Refused)
            }
        }
    }

    fn start() -> Option<(SyncSender<Command>, UnixStream)> {
        let (sender, commands) = mpsc::sync_channel(1);
        let (wake, receiver) = UnixStream::pair().ok()?;
        std::thread::Builder::new()
            .name("x11-selection-owner".into())
            .spawn(move || {
                if let Some(mut server) = Server::open() {
                    server.run(commands, receiver);
                }
            })
            .ok()?;
        Some((sender, wake))
    }
}

struct Port(RustConnection);

impl OwnershipPort for Port {
    fn set_owner(&mut self, window: u32, selection: u32, stamp: u32) -> bool {
        self.0
            .set_selection_owner(window, selection, stamp)
            .is_ok_and(|cookie| cookie.check().is_ok())
    }
    fn owner(&mut self, selection: u32) -> Option<u32> {
        Some(self.0.get_selection_owner(selection).ok()?.reply().ok()?.owner)
    }
}

impl OwnerPort for Port {
    fn intern(&mut self, name: &str) -> Option<u32> {
        Some(self.0.intern_atom(false, name.as_bytes()).ok()?.reply().ok()?.atom)
    }
    fn bytes(&mut self, dest: Destination, kind: u32, bytes: &[u8]) -> bool {
        self.0
            .change_property8(PropMode::REPLACE, dest.window, dest.property, kind, bytes)
            .is_ok_and(|cookie| cookie.check().is_ok())
    }
    fn words(&mut self, dest: Destination, kind: u32, words: &[u32]) -> bool {
        self.0
            .change_property32(PropMode::REPLACE, dest.window, dest.property, kind, words)
            .is_ok_and(|cookie| cookie.check().is_ok())
    }
    fn watch(&mut self, window: u32, enable: bool) -> bool {
        let mask = if enable { EventMask::PROPERTY_CHANGE } else { EventMask::NO_EVENT };
        self.0
            .change_window_attributes(window, &ChangeWindowAttributesAux::new().event_mask(mask))
            .is_ok_and(|cookie| cookie.check().is_ok())
    }
}

struct Server {
    port: Port,
    root: u32,
    clipboard: u32,
    offers: HashMap<u32, Active>,
}

impl Server {
    fn open() -> Option<Self> {
        let (connection, screen) = RustConnection::connect(None).ok()?;
        let root = connection.setup().roots.get(screen)?.root;
        let mut port = Port(connection);
        let clipboard = port.intern("CLIPBOARD")?;
        Some(Self { port, root, clipboard, offers: HashMap::new() })
    }

    fn run(&mut self, commands: Receiver<Command>, mut wake: UnixStream) -> Option<()> {
        loop {
            let now = Instant::now();
            self.offers.retain(|window, active| {
                active.owner.expire(&mut self.port, now);
                let live = active.owner.live(now);
                if !live {
                    // When: live is false, release the window and stop watching any expired requestors.
                    for cursor in &active.owner.cursors {
                        self.port.watch(cursor.dest.window, false);
                    }
                    let _ = self.port.0.destroy_window(*window);
                }
                live
            });
            // One event per iteration gives commands and expired cursors a turn under event flood.
            let event = self.port.0.poll_for_event().ok()?;
            let buffered = event.is_some();
            if let Some(event) = event {
                self.dispatch(event);
            }
            self.port.0.flush().ok()?;
            let deadline = self
                .offers
                .values()
                .flat_map(|active| {
                    active
                        .owner
                        .accepting
                        .then_some(active.owner.expires)
                        .into_iter()
                        .chain(active.owner.cursors.iter().map(|cursor| cursor.deadline))
                })
                .min();
            let timeout = if buffered {
                0
            } else {
                // When: buffered is false, block until socket activity or the nearest deadline.
                deadline.map_or(-1, |deadline| {
                    deadline.saturating_duration_since(Instant::now()).as_millis() as i32
                })
            };
            let mut sockets = [wake.as_raw_fd(), self.port.0.stream().as_raw_fd()]
                .map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 });
            let ready = wait_sockets(&mut sockets, timeout);
            if ready < 0 {
                // When: ready is negative, distinguish interruption from a broken poll.
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    // When: Interrupted, recompute deadlines before retrying the wait.
                    continue;
                }
                return None;
            }
            if sockets[0].revents != 0 {
                // When: sockets reports a command wake, one byte names a queued command; EOF ends the server.
                wake.read_exact(&mut [0]).ok()?;
                let command = commands.recv().ok()?;
                let targets = command.targets;
                let result = self.install(command.offer, command.lease, targets, command.binding);
                let _ = command.answer.send(
                    result.unwrap_or_else(|| WriteResult::each(targets, TargetOutcome::Refused)),
                );
            }
        }
    }

    fn install(
        &mut self,
        offer: NativeOffer,
        lease: ClipLease,
        targets: ClipTargets,
        binding: WriteBinding,
    ) -> Option<WriteResult> {
        let window = self.port.0.generate_id().ok()?;
        self.port
            .0
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                self.root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .ok()?
            .check()
            .ok()?;
        let result = self.install_on(window, offer, lease, targets, binding);
        if !self.offers.contains_key(&window) {
            // When: offers has no entry for window, neither selection needs it to serve data.
            let _ = self.port.0.destroy_window(window);
        }
        result
    }

    fn install_on(
        &mut self,
        window: u32,
        offer: NativeOffer,
        lease: ClipLease,
        targets: ClipTargets,
        binding: WriteBinding,
    ) -> Option<WriteResult> {
        // A property notification supplies real server time. Dispatch unrelated events rather
        // than losing an admitted reader's delete while waiting for the timestamp.
        self.port
            .0
            .change_property8(PropMode::REPLACE, window, AtomEnum::INTEGER, AtomEnum::INTEGER, &[])
            .ok()?
            .check()
            .ok()?;
        let stamp = loop {
            let event = self.port.0.wait_for_event().ok()?;
            if let Event::PropertyNotify(change) = &event {
                // When: PropertyNotify arrives, only our clock property supplies the acquisition time.
                if change.window == window && change.atom == u32::from(AtomEnum::INTEGER) {
                    // When: change names our clock property, zero cannot version an offer.
                    if change.time == 0 {
                        // When: time is zero, it is indistinguishable from CurrentTime.
                        return None;
                    }
                    break change.time;
                }
            }
            self.dispatch(event);
        };
        // ChangeProperty has a 24-byte header and BIG-REQUESTS can add another 4 bytes.
        let direct = self.port.0.maximum_request_bytes().checked_sub(28)? & !3;
        let owner = Owner::stage(&mut self.port, offer, lease, stamp, direct, Instant::now())?;
        // Fixed slots retain each destination's identity even for PRIMARY-only offers.
        let primary = u32::from(AtomEnum::PRIMARY);
        let selections = [
            if targets.clipboard { self.clipboard } else { 0 },
            if targets.primary { primary } else { 0 },
        ];
        Some(install_claim(
            &mut self.offers,
            &mut self.port,
            window,
            Active { owner, selections },
            &|| binding.may_commit(),
        ))
    }

    fn dispatch(&mut self, event: Event) {
        match event {
            Event::SelectionClear(clear) => {
                if let Some(active) = self.offers.get_mut(&clear.owner) {
                    active.retire(clear.selection);
                }
            }
            Event::SelectionRequest(request) => {
                // When: SelectionRequest arrives, answer even a refused conversion with SelectionNotify.
                let property =
                    if request.property == 0 { request.target } else { request.property };
                let dest = Destination { window: request.requestor, property };
                // One PropertyDelete must drive only one cursor, even across selection changes.
                let busy = self.offers.values().any(|active| {
                    active.owner.cursors.iter().any(|cursor| cursor.dest.window == dest.window)
                });
                let stored = !busy
                    && self.offers.get_mut(&request.owner).is_some_and(|active| {
                        active.accepts(request.selection)
                            && active.owner.request(
                                &mut self.port,
                                dest,
                                request.target,
                                Instant::now(),
                            )
                    });
                let notify = SelectionNotifyEvent {
                    response_type: xproto::SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: request.time,
                    requestor: request.requestor,
                    selection: request.selection,
                    target: request.target,
                    property: if stored { property } else { 0 },
                };
                let _ =
                    self.port.0.send_event(false, request.requestor, EventMask::NO_EVENT, notify);
            }
            Event::PropertyNotify(change) if change.state == xproto::Property::DELETE => {
                let dest = Destination { window: change.window, property: change.atom };
                for active in self.offers.values_mut() {
                    active.owner.deleted(&mut self.port, dest, Instant::now());
                }
            }
            _ => {
                // When: event is neither a selection event nor a property delete, no reader is waiting for it.
            }
        }
    }
}

/// Poll the command and server sockets together, without an idle wakeup loop.
fn wait_sockets(sockets: &mut [libc::pollfd; 2], timeout: i32) -> i32 {
    // SAFETY: sockets points to two writable pollfd records, with live borrowed descriptors for the duration of poll.
    unsafe { libc::poll(sockets.as_mut_ptr(), 2, timeout) }
}
