//! OSC 5522 write transactions above the parser: one per pane, owned by its VT worker.
//!
//! The parser raises one payload-free `VtEvent::KittyClipboard` per complete packet and queues a
//! valid packet's encoded payload in its kitty inbox. [`KittyClipboardTx`] keeps the pane's one
//! open write: each MIME representation is decoded into a buffer reserved from
//! [`ClipboardStaging`] before it is allocated, aliases are kept in order, and a commit resolves
//! the aliases over every staged representation and plans the native offer
//! (`kitty_clipboard_offer`): a type the platform cannot hold is omitted and released, text alone
//! is written as before, and text beside other types is published with them in one operation.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use sonicterm_cfg::config::{Osc52Policy, Osc52ReadPolicy};
use sonicterm_vt::vt::{
    kitty_clipboard_status_reply, ClipboardInbox, KittyClipboardLocation, KittyClipboardMetadata,
    KittyClipboardOperation, KittyClipboardRequest, KittyClipboardStatus,
    MAX_KITTY_CLIPBOARD_MIME_BYTES,
};

use super::kitty_clipboard_offer::{plan, platform_holds, NativeFormat, NativeOffer, OfferPlan};
use super::kitty_clipboard_read::{parse_read_request, read_payload_valid};
use super::osc52_read_grants::GrantCredential;
use super::osc52_read_ticket::{
    admit_kitty_read, admit_write, ConsentTicket, ReadIdentity, WriteAdmission,
};
use super::osc52_stream::{sextet, ClipLease, ClipTargets, ClipboardJob, ClipboardStaging};
use super::spawn_pane::Osc52Refusal;

/// How long an open write may wait for its next packet before it ends `EBUSY`.
pub(in crate::app) const KITTY_WRITE_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long an open write may stay open in all before it ends `EBUSY`.
pub(in crate::app) const KITTY_WRITE_ABSOLUTE_TIMEOUT: Duration = Duration::from_secs(600);

/// Most MIME representations one write stages; later types are ignored.
pub(in crate::app) const MAX_KITTY_WRITE_MIMES: usize = 64;

/// Most aliases one write keeps; later aliases are ignored.
pub(in crate::app) const MAX_KITTY_WRITE_ALIASES: usize = 64;

/// Where a write's statuses go: the opening `write`'s sanitized id and location, and the
/// terminator of the packet being answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) struct KittyReply {
    id: String,
    location: KittyClipboardLocation,
    bel: bool,
    /// The opening packet's `name` and `pw`, which session grants are looked up by.
    credential: Option<GrantCredential>,
}

impl KittyReply {
    /// The reply address `metadata` opens, answered with `bel`'s terminator.
    fn of(metadata: &KittyClipboardMetadata, bel: bool) -> Self {
        let credential = GrantCredential::new(&metadata.name, &metadata.password);
        Self { id: metadata.id.clone(), location: metadata.location, bel, credential }
    }

    /// The location the write targets.
    pub(in crate::app) fn location(&self) -> KittyClipboardLocation {
        self.location
    }

    /// The credential the opening packet offered, if both its `name` and `pw` were nonempty.
    pub(in crate::app) fn credential(&self) -> Option<&GrantCredential> {
        self.credential.as_ref()
    }

    /// The `type=write` status reply carrying `status`.
    pub(in crate::app) fn status(&self, status: KittyClipboardStatus) -> Vec<u8> {
        let write = KittyClipboardOperation::Write;
        kitty_clipboard_status_reply(write, status, self.location, &self.id, self.bel)
    }
}

/// What one packet, deadline or policy change produced for the VT worker.
pub(in crate::app) enum KittyStep {
    /// Nothing to send.
    Silent,
    /// One status reply for the pane's reply spool; it carries no clipboard data.
    Reply(Vec<u8>),
    /// A committed write to post to the event loop under `allow`.
    Post(ClipboardJob),
    /// A committed write held on a consent ticket under `ask`.
    Ticket(ConsentTicket),
}

impl std::fmt::Debug for KittyStep {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Only lengths are printed, so a step can be logged without its clipboard text.
        match self {
            Self::Silent => formatter.write_str("Silent"),
            Self::Reply(bytes) => write!(formatter, "Reply({:?})", String::from_utf8_lossy(bytes)),
            Self::Post(job) => write!(formatter, "Post(text_bytes: {})", job.text.len()),
            Self::Ticket(_) => formatter.write_str("Ticket"),
        }
    }
}

/// One MIME type's strict Base64 stream across packets: a group carries at most 3 sextets between
/// packets and padding closes it, so a packet ending on padding leaves the next one starting fresh.
#[derive(Clone, Copy, Default)]
struct Base64Group {
    sextets: [u8; 4],
    filled: usize,
    padding: usize,
}

impl Base64Group {
    /// Decode `chunk`, one packet's payload, appending to `output` when given; returns the decoded
    /// length, or `Err` on a byte outside the alphabet, misplaced padding, data after a padded group
    /// in the same packet, or a padded group whose unused low bits are not zero.
    fn run(&mut self, chunk: &[u8], mut output: Option<&mut Vec<u8>>) -> Result<usize, ()> {
        let mut produced = 0;
        let mut padded_group_ended = false;
        for byte in chunk {
            if padded_group_ended {
                // When: padded_group_ended, a padded group ends its packet; only the next packet
                // may start a new group.
                return Err(());
            }
            if *byte == b'=' {
                // When: byte is `=`, it is padding: checked and counted, never decoded.
                if self.filled < 2 || self.filled + self.padding >= 4 {
                    // When: padding follows fewer than two sextets or overfills the group, the
                    // stream is malformed.
                    return Err(());
                }
                self.padding += 1;
            } else {
                // When: byte is not padding, it must be a sextet with no padding before it.
                let value = sextet(*byte).ok_or(())?;
                if self.padding > 0 {
                    // When: data follows padding inside one group, the stream is malformed.
                    return Err(());
                }
                self.sextets[self.filled] = value;
                self.filled += 1;
            }
            if self.filled + self.padding == 4 {
                // When: filled plus padding reaches 4, the group is whole: checked, then decoded.
                let [first, second, third, fourth] = self.sextets;
                // The sextet before the padding carries bits no byte uses; strict Base64 has them 0.
                let unused_bits = match self.filled {
                    2 => second & 0x0f,
                    3 => third & 0x03,
                    _ => 0,
                };
                if unused_bits != 0 {
                    // When: unused_bits are set, the group is not canonical Base64.
                    return Err(());
                }
                let decoded =
                    [first << 2 | second >> 4, second << 4 | third >> 2, third << 6 | fourth];
                let byte_count = self.filled - 1;
                if let Some(output) = output.as_deref_mut() {
                    output.extend_from_slice(&decoded[..byte_count]);
                }
                produced += byte_count;
                padded_group_ended = self.padding > 0;
                *self = Self::default();
            }
        }
        Ok(produced)
    }

    /// Whether the stream ended on a whole group, as the spec's required padding makes it.
    fn complete(&self) -> bool {
        self.filled == 0 && self.padding == 0
    }
}

/// One staged representation.
struct Entry {
    mime: String,
    data: Vec<u8>,
}

/// The pane's open write.
struct OpenWrite {
    reply: KittyReply,
    /// The decoded-byte limit read when the write opened.
    limit_bytes: usize,
    entries: Vec<Entry>,
    /// `(alias, target)` pairs in arrival order; a repeated alias keeps its place.
    aliases: Vec<(String, String)>,
    /// The entry receiving data.
    current: Option<usize>,
    group: Base64Group,
    /// Covers every entry's capacity and every alias.
    lease: ClipLease,
    opened_at: Instant,
    last_packet: Instant,
    /// The policy generation the write opened under; any `osc52`/`osc52_read` change ends it.
    policy_generation: u64,
}

impl OpenWrite {
    /// End the current entry's stream; `Err` when it did not end on a whole group.
    fn finish_current(&mut self) -> Result<(), KittyClipboardStatus> {
        let complete = self.group.complete();
        self.group = Base64Group::default();
        self.current = None;
        complete.then_some(()).ok_or(KittyClipboardStatus::Einval)
    }

    /// Stage one `wdata` chunk of `mime`.
    fn append(&mut self, mime: &str, chunk: &[u8]) -> Result<(), KittyClipboardStatus> {
        if self.current.is_some_and(|index| self.entries[index].mime != mime) {
            // The chunk switches MIME type, so the previous stream ends here.
            self.finish_current()?;
        }
        if self.current.is_none() {
            // When: current is none, a repeated mime restarts its region, else a new entry opens.
            if let Some(index) = self.entries.iter().position(|entry| entry.mime == mime) {
                self.entries[index].data.clear();
                self.current = Some(index);
            } else if self.entries.len() < MAX_KITTY_WRITE_MIMES {
                // When: entries is below MAX_KITTY_WRITE_MIMES, the new type takes an entry.
                if !self.lease.grow_to(self.lease.bytes() + mime.len()) {
                    // When: lease.grow_to fails for the mime name, the write is busy.
                    return Err(KittyClipboardStatus::Ebusy);
                }
                self.entries.push(Entry { mime: mime.to_owned(), data: Vec::new() });
                self.current = Some(self.entries.len() - 1);
            } else {
                // When: every entry slot is taken, this type's data is ignored, as Ghostty does.
                return Ok(());
            }
        }
        let index = self.current.expect("an entry receives data");
        // A dry run validates the whole chunk and gives its exact length before anything grows.
        let decoded =
            self.group.clone().run(chunk, None).map_err(|()| KittyClipboardStatus::Einval)?;
        let staged: usize = self.entries.iter().map(|entry| entry.data.len()).sum();
        if staged + decoded > self.limit_bytes {
            // When: staged plus decoded passes limit_bytes, the write ends `EFBIG` before allocating.
            return Err(KittyClipboardStatus::Efbig);
        }
        let data = &mut self.entries[index].data;
        let needed = data.len() + decoded;
        if needed > data.capacity() {
            // When: needed passes data.capacity(), the growth is reserved first: doubled within
            // the limit when the lease allows it, else exactly needed.
            let doubled = needed.max(data.capacity() * 2).min(self.limit_bytes).max(needed);
            let base = self.lease.bytes() - data.capacity();
            let target = if self.lease.grow_to(base + doubled) {
                doubled
            } else if self.lease.grow_to(base + needed) {
                // When: lease.grow_to covers needed but not doubled, exactly needed is reserved.
                needed
            } else {
                // When: lease.grow_to covers neither doubled nor needed, the write is busy.
                return Err(KittyClipboardStatus::Ebusy);
            };
            data.reserve_exact(target - data.len());
            if !self.lease.grow_to(base + data.capacity()) {
                // When: lease.grow_to cannot cover the capacity reserve_exact gave, the write is busy.
                return Err(KittyClipboardStatus::Ebusy);
            }
        }
        self.group.run(chunk, Some(data)).map(|_| ()).map_err(|()| KittyClipboardStatus::Einval)
    }

    /// Record the aliases a `walias` chunk names for `target`.
    fn alias(&mut self, target: &str, chunk: &[u8]) -> Result<(), KittyClipboardStatus> {
        let base = self.lease.bytes();
        if !self.lease.grow_to(base + chunk.len()) {
            // When: lease.grow_to cannot cover decoding the chunk, the write is busy.
            return Err(KittyClipboardStatus::Ebusy);
        }
        let decoded = base64::engine::general_purpose::STANDARD.decode(chunk);
        let names = decoded.ok().and_then(|bytes| String::from_utf8(bytes).ok());
        // The decoded names stay reserved while they are read below; the scope releases them.
        let result = self.record_aliases(target, names.as_deref());
        self.lease.shrink_to(self.lease.bytes() - chunk.len());
        result
    }

    /// Record each alias `names` lists for `target`, every copy reserved before it is made.
    fn record_aliases(
        &mut self,
        target: &str,
        names: Option<&str>,
    ) -> Result<(), KittyClipboardStatus> {
        let names = names.ok_or(KittyClipboardStatus::Einval)?;
        for name in names.split_ascii_whitespace() {
            if name.len() > MAX_KITTY_CLIPBOARD_MIME_BYTES {
                // When: name is over the MIME bound, it is skipped.
                continue;
            }
            if let Some(index) = self.aliases.iter().position(|(alias, _)| alias == name) {
                // When: aliases already holds name, it points at the newer target, keeping its
                // place; the new target is reserved, by length, before it replaces the old one.
                let old_target = self.aliases[index].1.len();
                let bytes = self.lease.bytes();
                if !self.lease.grow_to(bytes + target.len()) {
                    // When: lease.grow_to cannot cover the new target, the write is busy.
                    return Err(KittyClipboardStatus::Ebusy);
                }
                self.aliases[index].1 = target.to_owned();
                self.lease.shrink_to(self.lease.bytes() - old_target);
                continue;
            }
            if self.aliases.len() == MAX_KITTY_WRITE_ALIASES {
                // When: every alias slot is taken, further aliases are ignored.
                return Ok(());
            }
            if !self.lease.grow_to(self.lease.bytes() + name.len() + target.len()) {
                // When: lease.grow_to cannot cover the alias name and target, the write is busy.
                return Err(KittyClipboardStatus::Ebusy);
            }
            self.aliases.push((name.to_owned(), target.to_owned()));
        }
        Ok(())
    }
}

/// Whether `mime` names plain text: `text/plain`, or `text/plain;` with parameters of printable
/// ASCII other than space (`charset=utf-8`), as programs offer it.
pub(in crate::app) fn is_text(mime: &str) -> bool {
    match mime.strip_prefix("text/plain") {
        Some("") => true,
        Some(rest) => rest.strip_prefix(';').is_some_and(|parameters| {
            !parameters.is_empty() && parameters.bytes().all(|byte| byte.is_ascii_graphic())
        }),
        None => false,
    }
}

/// The most `plan` allocates for names of `name_lengths` (every staged type, then every alias):
/// per name, its name-table slot, remap slot, representation slot and kept-buffer slot, and a
/// copy of the name when it becomes a custom format.
pub(in crate::app) fn plan_workspace_bytes(name_lengths: impl Iterator<Item = usize>) -> usize {
    let per_name = std::mem::size_of::<(&str, usize)>()
        + std::mem::size_of::<Option<usize>>()
        + std::mem::size_of::<(NativeFormat, usize)>()
        + std::mem::size_of::<Vec<u8>>();
    name_lengths.map(|length| per_name + length).sum()
}

/// Size `lease` to exactly `total`, growing only when the budget covers it; false when it cannot.
fn size_lease(lease: &mut ClipLease, total: usize) -> bool {
    if total <= lease.bytes() {
        // When: total fits in what the lease holds, the surplus is released.
        lease.shrink_to(total);
        return true;
    }
    lease.grow_to(total)
}

/// One pane's OSC 5522 write transaction, driven by its VT worker outside the parser lock.
pub(in crate::app) struct KittyClipboardTx {
    staging: Arc<ClipboardStaging>,
    identity: Arc<ReadIdentity>,
    pane_id: u64,
    open: Option<OpenWrite>,
    /// Which native formats a commit may publish; the platform's, unless a test narrows it.
    holds: fn(&NativeFormat, &[u8]) -> bool,
}

impl Clone for KittyClipboardTx {
    /// A transaction with no write open on the same staging, identity and pane: an open write and
    /// its lease belong to the one worker that tracks it.
    fn clone(&self) -> Self {
        let mut tx = Self::new(self.staging.clone(), self.identity.clone());
        tx.pane_id = self.pane_id;
        tx
    }
}

/// One request a parser section raised, with the command boundaries passed before it and its
/// position on the wire; `None` is a RIS, which releases the open write silently.
pub(in crate::app) type KittySectionRequest =
    Option<(u64, KittyClipboardRequest, bool, u64, usize)>;

impl KittyClipboardTx {
    /// A transaction with no write open, reserving from `staging` and binding tickets to `identity`.
    pub(in crate::app) fn new(staging: Arc<ClipboardStaging>, identity: Arc<ReadIdentity>) -> Self {
        Self { staging, identity, pane_id: 0, open: None, holds: platform_holds }
    }

    /// Publish only the formats `holds` accepts, instead of the platform's.
    #[cfg(any(test, feature = "test-support"))]
    pub(in crate::app) fn set_holds(&mut self, holds: fn(&NativeFormat, &[u8]) -> bool) {
        self.holds = holds;
    }

    /// Name `pane_id` in the tickets this transaction admits.
    pub(in crate::app) fn bind_pane(&mut self, pane_id: u64) {
        self.pane_id = pane_id;
    }

    /// Reserve from `staging` instead, dropping any open write.
    #[cfg(any(test, feature = "test-support"))]
    pub(in crate::app) fn replace_staging(&mut self, staging: Arc<ClipboardStaging>) {
        self.open = None;
        self.staging = staging;
    }

    /// The staging this transaction reserves from and reads the `osc52` policy of.
    pub(in crate::app) fn staging(&self) -> &Arc<ClipboardStaging> {
        &self.staging
    }

    /// Whether a write is open.
    #[cfg(test)]
    pub(in crate::app) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Release the open write without a reply: RIS, or the pane retiring.
    pub(in crate::app) fn release(&mut self) {
        self.open = None;
    }

    /// When the open write expires: 30 s after its last packet, 10 min after it opened.
    pub(in crate::app) fn deadline(&self) -> Option<Instant> {
        self.open.as_ref().map(|open| {
            (open.last_packet + KITTY_WRITE_IDLE_TIMEOUT)
                .min(open.opened_at + KITTY_WRITE_ABSOLUTE_TIMEOUT)
        })
    }

    /// Finish the open write at `now` when the policy is now `deny` or changed since it opened, even
    /// back to the same policy (`EPERM`), or its deadline passed (`EBUSY`); returns the status
    /// reply. Runs before every ready chunk and while the child prints nothing.
    pub(in crate::app) fn service(&mut self, now: Instant) -> Option<Vec<u8>> {
        let deadline = self.deadline()?;
        let generation = self.staging.policy_generation();
        let moved = self.open.as_ref().is_some_and(|open| open.policy_generation != generation);
        let status = if moved || self.staging.write_policy() == Osc52Policy::Deny {
            KittyClipboardStatus::Eperm
        } else if now >= deadline {
            // When: now reaches the deadline, the write expired.
            KittyClipboardStatus::Ebusy
        } else {
            // When: the policy still admits the write and its deadline is ahead, it stays open.
            return None;
        };
        self.open.take().map(|open| open.reply.status(status))
    }

    /// Run one section's requests in order, each at the command epoch of its position; every step
    /// carries its request's wire position.
    pub(in crate::app) fn run(
        &mut self,
        inbox: Option<&ClipboardInbox>,
        requests: Vec<KittySectionRequest>,
        now: Instant,
    ) -> Vec<(usize, KittyStep)> {
        let base_epoch = self.identity.command_epoch();
        let mut steps = Vec::new();
        for request in requests {
            let Some((stream, request, bel, passed, position)) = request else {
                // When: the section passed a RIS here, the open write ends without a reply and
                // every session grant bound before the reset is revoked.
                self.release();
                self.identity.bump_reset();
                continue;
            };
            let Some(inbox) = inbox else {
                // When: no inbox was taken, the section raised no packet to read from it.
                continue;
            };
            let step = self.handle(inbox, stream, request, bel, base_epoch + passed, now);
            steps.push((position, step));
        }
        steps
    }

    /// Apply one packet.
    pub(in crate::app) fn handle(
        &mut self,
        inbox: &ClipboardInbox,
        stream: u64,
        request: KittyClipboardRequest,
        bel: bool,
        command_epoch: u64,
        now: Instant,
    ) -> KittyStep {
        let (metadata, payload) = match request {
            KittyClipboardRequest::Packet { metadata, payload } => (metadata, payload),
            KittyClipboardRequest::Overflow(operation) => {
                // When: the request is Overflow, an open write ends `EFBIG` for wdata/walias.
                return self.fail_open(operation, KittyClipboardStatus::Efbig, bel);
            }
            KittyClipboardRequest::InvalidValue(operation) => {
                // When: the request is InvalidValue, an open write ends `EINVAL` for wdata/walias.
                return self.fail_open(operation, KittyClipboardStatus::Einval, bel);
            }
        };
        // A packet with a payload has one chunk in the inbox under `stream`; it leaves the inbox
        // now, whatever becomes of the packet.
        let chunk = payload.then(|| inbox.pop(stream)).flatten().unwrap_or_default();
        match metadata.operation {
            KittyClipboardOperation::Read => self.read(&metadata, &chunk, bel, command_epoch, now),
            KittyClipboardOperation::Write => self.begin(&metadata, bel, now),
            KittyClipboardOperation::Wdata if metadata.mime.is_empty() => {
                self.commit(bel, command_epoch, now)
            }
            KittyClipboardOperation::Wdata => {
                self.step(bel, now, |open| open.append(&metadata.mime, &chunk))
            }
            KittyClipboardOperation::Walias => {
                // When: the operation is Walias, the chunk lists aliases for metadata.mime.
                if metadata.mime.is_empty() {
                    // When: metadata.mime is empty, the alias has no target and the write ends `EINVAL`.
                    return self.step(bel, now, |_| Err(KittyClipboardStatus::Einval));
                }
                self.step(bel, now, |open| open.alias(&metadata.mime, &chunk))
            }
        }
    }

    /// Admit one read: an undecodable payload is dropped first, before the budget or the policy
    /// could answer it; `loc=primary` off Linux is `ENOSYS`, and otherwise the read takes a ticket
    /// or is answered as [`admit_kitty_read`] decides. Decoding the payload is reserved from the
    /// staging first, so a full budget answers `EBUSY`.
    fn read(
        &mut self,
        metadata: &KittyClipboardMetadata,
        chunk: &[u8],
        bel: bool,
        command_epoch: u64,
        now: Instant,
    ) -> KittyStep {
        let status = |status| {
            let read = KittyClipboardOperation::Read;
            kitty_clipboard_status_reply(read, status, metadata.location, &metadata.id, bel)
        };
        if !read_payload_valid(chunk) {
            // When: read_payload_valid refuses the payload, it is malformed and dropped silently,
            // before the budget or the policy could choose a status.
            return KittyStep::Silent;
        }
        let denied = self.staging.read_policy() == Osc52ReadPolicy::Deny;
        let mut lease = self.staging.lease();
        if !lease.grow_to(chunk.len()) {
            // When: lease.grow_to cannot cover decoding the payload, the read is busy, or `EPERM`
            // under `deny`, which decides a read before the budget does.
            let refusal =
                if denied { KittyClipboardStatus::Eperm } else { KittyClipboardStatus::Ebusy };
            return KittyStep::Reply(status(refusal));
        }
        let Some(request) = parse_read_request(metadata, chunk, bel) else {
            // When: parse_read_request refuses the payload, the read is dropped with no reply.
            return KittyStep::Silent;
        };
        drop(lease);
        if denied {
            // When: denied, a valid read answers `EPERM` before its location is checked.
            return KittyStep::Reply(status(KittyClipboardStatus::Eperm));
        }
        let primary = metadata.location == KittyClipboardLocation::Primary;
        if primary && !cfg!(target_os = "linux") {
            // When: primary is asked for off Linux, the location does not exist.
            return KittyStep::Reply(status(KittyClipboardStatus::Enosys));
        }
        let (staging, identity) = (&self.staging, &self.identity);
        match admit_kitty_read(staging, identity, self.pane_id, command_epoch, request, now) {
            Ok(ticket) => KittyStep::Ticket(ticket),
            Err(reply) => KittyStep::Reply(reply),
        }
    }

    /// Finish an open write with `status` for a `wdata`/`walias` packet; anything else is dropped.
    fn fail_open(
        &mut self,
        operation: KittyClipboardOperation,
        status: KittyClipboardStatus,
        bel: bool,
    ) -> KittyStep {
        let ends_write =
            matches!(operation, KittyClipboardOperation::Wdata | KittyClipboardOperation::Walias);
        match self.open.take() {
            Some(mut open) if ends_write => {
                open.reply.bel = bel;
                KittyStep::Reply(open.reply.status(status))
            }
            open => {
                // No write is open, or the packet is a read or write: it is dropped and an open
                // write stays as it was.
                self.open = open;
                KittyStep::Silent
            }
        }
    }

    /// Open a write, silently replacing one already open; under `deny` answer `EPERM` at once.
    fn begin(&mut self, metadata: &KittyClipboardMetadata, bel: bool, now: Instant) -> KittyStep {
        self.open = None;
        let reply = KittyReply::of(metadata, bel);
        // Sampled before the policy, so a reload racing the open leaves a generation that moves.
        let policy_generation = self.staging.policy_generation();
        if self.staging.write_policy() == Osc52Policy::Deny {
            // When: write_policy is Deny, the write can never succeed, so nothing is staged.
            return KittyStep::Reply(reply.status(KittyClipboardStatus::Eperm));
        }
        self.open = Some(OpenWrite {
            reply,
            limit_bytes: self.staging.stream_limit(),
            entries: Vec::new(),
            aliases: Vec::new(),
            current: None,
            group: Base64Group::default(),
            lease: self.staging.lease(),
            opened_at: now,
            last_packet: now,
            policy_generation,
        });
        KittyStep::Silent
    }

    /// Apply `change` to the open write; on an error the write ends with that status.
    fn step(
        &mut self,
        bel: bool,
        now: Instant,
        change: impl FnOnce(&mut OpenWrite) -> Result<(), KittyClipboardStatus>,
    ) -> KittyStep {
        let Some(open) = self.open.as_mut() else {
            // When: no write is open, data and aliases are ignored, as kitty ignores them.
            return KittyStep::Silent;
        };
        open.last_packet = now;
        match change(open) {
            Ok(()) => KittyStep::Silent,
            Err(status) => self.fail_open(KittyClipboardOperation::Wdata, status, bel),
        }
    }

    /// Commit the open write: check its deadline against the clock, resolve aliases, choose the
    /// text, check the policy and its opening generation again and route it.
    fn commit(&mut self, bel: bool, command_epoch: u64, now: Instant) -> KittyStep {
        let Some(mut open) = self.open.take() else {
            // When: no write is open, a commit is ignored.
            return KittyStep::Silent;
        };
        open.reply.bel = bel;
        let deadline = (open.last_packet + KITTY_WRITE_IDLE_TIMEOUT)
            .min(open.opened_at + KITTY_WRITE_ABSOLUTE_TIMEOUT);
        if now.max(Instant::now()) >= deadline {
            // When: the clock reaches deadline at the commit, the write expired while its section
            // was parsed, so one section timestamp never carries it past its deadline: `EBUSY`.
            return KittyStep::Reply(open.reply.status(KittyClipboardStatus::Ebusy));
        }
        if open.current.is_some() && open.finish_current().is_err() {
            // When: finish_current fails, the last stream ended mid-group and the write is `EINVAL`.
            return KittyStep::Reply(open.reply.status(KittyClipboardStatus::Einval));
        }
        let OpenWrite { reply, entries, aliases, mut lease, policy_generation, .. } = open;
        let primary = reply.location == KittyClipboardLocation::Primary;
        if primary && !cfg!(target_os = "linux") {
            // When: primary is set off Linux, there is no PRIMARY and the commit is `ENOSYS`.
            return KittyStep::Reply(reply.status(KittyClipboardStatus::Enosys));
        }
        // Planning's own allocations (its name table, index maps and custom format names) are
        // reserved first at their worst case; the reservation ends once the plan is made.
        let names = entries.iter().map(|entry| entry.mime.len());
        let plan_bytes =
            plan_workspace_bytes(names.chain(aliases.iter().map(|(alias, _)| alias.len())));
        if !lease.grow_to(lease.bytes() + plan_bytes) {
            // When: lease.grow_to cannot cover planning's workspace, the write is busy.
            return KittyStep::Reply(reply.status(KittyClipboardStatus::Ebusy));
        }
        let entries = entries.into_iter().map(|entry| (entry.mime, entry.data)).collect();
        let planned = plan(entries, &aliases, &self.holds);
        lease.shrink_to(lease.bytes() - plan_bytes);
        let offer = match planned {
            OfferPlan::Offer(offer) => Some(offer),
            OfferPlan::Clear => None,
            OfferPlan::Unavailable => {
                // When: plan is Unavailable, every name was omitted and the commit is `ENOSYS`.
                return KittyStep::Reply(reply.status(KittyClipboardStatus::Enosys));
            }
        };
        if let Some(omitted) = offer.as_ref().map(NativeOffer::omitted).filter(|count| *count > 0) {
            tracing::debug!(omitted, "OSC 5522 write omits types this clipboard cannot hold");
        }
        let moved = self.staging.policy_generation() != policy_generation;
        if moved || self.staging.write_policy() == Osc52Policy::Deny {
            // When: moved (a policy change since `write`) or write_policy is Deny at the commit,
            // the write is refused `EPERM`, as the settle step refuses it.
            return KittyStep::Reply(reply.status(KittyClipboardStatus::Eperm));
        }
        let targets = ClipTargets { clipboard: !primary, primary };
        let job = match offer {
            Some(offer) if !offer.is_text_only() => {
                // When: offer holds more than text, its buffers and native copies are reserved
                // before it is posted, so a publish never allocates past the budget.
                if !size_lease(&mut lease, offer.reserved_bytes()) {
                    // When: size_lease cannot cover offer.reserved_bytes, the write is busy.
                    return KittyStep::Reply(reply.status(KittyClipboardStatus::Ebusy));
                }
                ClipboardJob::kitty_offer(&self.staging, offer, lease, targets, reply.clone())
            }
            offer => {
                // Text alone, or a clear, takes the text path as before.
                let text = offer.map(NativeOffer::into_text).unwrap_or_default();
                ClipboardJob::kitty(&self.staging, text, lease, targets, reply.clone())
            }
        };
        let job = match job {
            Ok(job) => job,
            Err(Osc52Refusal::Busy) => {
                // When: the refusal is Busy, the budget or the program slots are full.
                return KittyStep::Reply(reply.status(KittyClipboardStatus::Ebusy));
            }
            Err(_) => {
                // When: job is any other refusal, its text could not be staged.
                return KittyStep::Reply(reply.status(KittyClipboardStatus::Eio));
            }
        };
        let (staging, identity, epoch) = (&self.staging, &self.identity, command_epoch);
        match admit_write(staging, identity, self.pane_id, epoch, policy_generation, job, now) {
            WriteAdmission::Post(job) => KittyStep::Post(job),
            WriteAdmission::Ticket(ticket) => KittyStep::Ticket(ticket),
            WriteAdmission::Busy => KittyStep::Reply(reply.status(KittyClipboardStatus::Ebusy)),
        }
    }
}

impl super::App {
    /// Answer a settled OSC 5522 write on pane `pane_id`'s PTY: `DONE` when written, `EIO` on a
    /// native failure, `EPERM` when the policy or the user refused it, `EFBIG` when too large.
    /// It is sent only while `binding`'s program still reads the pane, so a status never becomes
    /// input to a later command. The reply carries no clipboard data, so it rides a zero-byte
    /// custody.
    pub(in crate::app) fn send_kitty_status(
        &mut self,
        pane_id: u64,
        reply: Option<KittyReply>,
        binding: &super::osc52_read_ticket::WriteBinding,
        outcome: super::event_loop::ClipWriteOutcome,
    ) {
        use super::event_loop::ClipWriteOutcome;
        let Some(reply) = reply else {
            // When: reply is None, the write came from OSC 52 or was dropped, so nothing is sent.
            return;
        };
        if !self.write_status_owed(pane_id, binding) {
            // When: write_status_owed is false, the program that asked no longer reads the pane.
            tracing::debug!(pane_id, "OSC 5522 write status withheld: its program is gone");
            return;
        }
        let status = match outcome {
            ClipWriteOutcome::Written => KittyClipboardStatus::Done,
            ClipWriteOutcome::Failed => KittyClipboardStatus::Eio,
            ClipWriteOutcome::Denied => KittyClipboardStatus::Eperm,
            ClipWriteOutcome::TooLarge => KittyClipboardStatus::Efbig,
        };
        let bytes = reply.status(status);
        let sent = self
            .send_read_reply(pane_id, sonicterm_io::pty::SensitiveReply::new(bytes, Box::new(())));
        if sent.is_err() {
            // The pane's writer is gone or busy: the status is dropped and only its name logged.
            tracing::debug!(pane_id, ?status, "OSC 5522 write status dropped");
        }
    }

    /// Whether a write's status still has its asker on pane `pane_id`: the pane's identity names
    /// the same PTY incarnation and command `binding` was admitted under, and a live pane still
    /// runs that PTY. A closed pane owes nothing it could deliver; its send fails on its own.
    pub(in crate::app) fn write_status_owed(
        &self,
        pane_id: u64,
        binding: &super::osc52_read_ticket::WriteBinding,
    ) -> bool {
        if !binding.asker_reads() {
            // When: asker_reads is false, the PTY or command changed since the write was admitted.
            return false;
        }
        let Some(source) = binding.source() else {
            // When: the write has no source pane, nothing can have changed under it.
            return true;
        };
        self.pane_by_id(pane_id).is_none_or(|pane| {
            pane.pty.as_ref().map_or(0, |pty| pty.incarnation()) == source.incarnation
        })
    }
}

#[cfg(test)]
#[path = "kitty_clipboard_tx_tests.rs"]
mod kitty_clipboard_tx_tests;
