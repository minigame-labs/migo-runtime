//! The single door frames come in through, and the credit accounting behind it.
//!
//! One entry point, deliberately. The C ABI exposes exactly one function that
//! accepts frame bytes; a second path -- a debug shortcut, a "fast" variant --
//! would be a second place for the nonce, generation and sequence rules to be
//! almost right.
//!
//! The rules here are the ones the parser cannot check, because they depend on
//! state the host owns: which launch these bytes belong to, which generation is
//! current, which sequence number comes next, whether the resource table is
//! ready, and how many frames the renderer is already holding.

use std::sync::Arc;

use crate::{
    MAX_TOTAL_BYTES, WireError,
    pool::{CreditWindow, FramePool, PooledFrame},
    validate,
};

/// What the host should do with the packet it just handed over.
///
/// The numbers cross the C ABI. Never renumber; only append.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum IngressDecision {
    /// Taken, and a credit consumed until the renderer reports completion.
    Accepted = 1,
    /// Legal, but there is no credit. The producer must wait, not retry
    /// immediately and not drop the packet: it may carry state or resource
    /// changes that a later frame depends on.
    WouldBlock = 2,
    /// Malformed, or not addressed to this session. Costs no credit, and the
    /// producer must not resend the same bytes.
    Rejected = 3,
    /// Correct bytes for a runtime generation that no longer exists -- the
    /// WebContent process was replaced, or the session was reloaded. Not an
    /// error on anyone's part, and not something a retry fixes.
    GenerationLost = 4,
}

/// The full answer, including what the producer needs to schedule the next one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IngressOutcome {
    pub decision: IngressDecision,
    pub remaining_credits: u32,
    /// Non-zero only for [`IngressDecision::Accepted`].
    pub accepted_sequence: u64,
    /// Non-zero only for [`IngressDecision::Rejected`], and stable across
    /// releases so production telemetry can tell the failures apart.
    pub wire_error_code: u32,
}

impl IngressOutcome {
    fn rejected(error: WireError, remaining_credits: u32) -> Self {
        Self {
            decision: IngressDecision::Rejected,
            remaining_credits,
            accepted_sequence: 0,
            wire_error_code: error.code(),
        }
    }

    fn refused(code: u32, remaining_credits: u32) -> Self {
        Self {
            decision: IngressDecision::Rejected,
            remaining_credits,
            accepted_sequence: 0,
            wire_error_code: code,
        }
    }
}

/// Reasons a packet is rejected that are not about its bytes.
///
/// Numbered above [`WireError`]'s range so a single telemetry field can carry
/// either without ambiguity.
pub const INGRESS_ERROR_FOREIGN_SESSION: u32 = 1001;
pub const INGRESS_ERROR_NONCONTIGUOUS_SEQUENCE: u32 = 1002;
pub const INGRESS_ERROR_STALE_SURFACE: u32 = 1003;
pub const INGRESS_ERROR_STALE_RESOURCE_EPOCH: u32 = 1004;
pub const INGRESS_ERROR_PACKET_TOO_LARGE: u32 = 1005;
pub const INGRESS_ERROR_RESOURCES_NOT_READY: u32 = 1006;

/// Every code above, for consumers that must cover all of them.
///
/// Same gate as [`crate::WireError::ALL`]: the document-agreement test parses
/// the `INGRESS_ERROR_*` constants out of this file's source and fails if this
/// list is missing one or has one twice, so nothing here is a list someone has
/// to remember to extend.
pub const INGRESS_ERROR_CODES: &[u32] = &[
    INGRESS_ERROR_FOREIGN_SESSION,
    INGRESS_ERROR_NONCONTIGUOUS_SEQUENCE,
    INGRESS_ERROR_STALE_SURFACE,
    INGRESS_ERROR_STALE_RESOURCE_EPOCH,
    INGRESS_ERROR_PACKET_TOO_LARGE,
    INGRESS_ERROR_RESOURCES_NOT_READY,
];

/// The first code in the identity/ordering range. Envelope failures are below
/// it, so one telemetry field carries either without ambiguity.
pub const INGRESS_ERROR_BASE: u32 = 1001;

/// Default and maximum credit depth, and they are the same number on purpose.
///
/// Two, so the producer can be building frame N+1 while the renderer works on
/// N, and no deeper: every additional credit is another frame of input latency
/// and another packet's worth of memory in flight.
///
/// [`FrameIngress::with_max_credits`] clamps into `1..=MAX_CREDITS` rather than
/// trusting its argument, so a value arriving from configuration, a remote
/// policy or a content manifest can only tighten the window. Raising the
/// ceiling is a code change with device measurements behind it -- which is what
/// "the right depth is a measurement" has to mean if it is not to become "the
/// right depth is whatever the caller passed".
pub const MAX_CREDITS: u32 = 2;
pub const DEFAULT_MAX_CREDITS: u32 = MAX_CREDITS;

/// Accepts frames for one runtime generation of one session.
///
/// A new generation gets a new `FrameIngress`. Nothing here is reset in place:
/// resetting would mean a packet in flight from the old generation could be
/// accepted by the new one, which is exactly the confusion generations exist to
/// prevent.
#[derive(Debug)]
pub struct FrameIngress {
    launch_nonce: u128,
    runtime_generation: u64,
    surface_generation: u64,
    resource_epoch: u64,
    resources_ready: bool,
    max_packet_bytes: u32,
    /// Shared with every frame in flight, because a completion token has to
    /// return its credit from wherever the renderer finished.
    credits: Arc<CreditWindow>,
    pool: Arc<FramePool>,
    last_accepted_sequence: u64,
}

impl FrameIngress {
    /// `launch_nonce` is the 128-bit identity generated once per app launch and
    /// paired with this ingress; `runtime_generation` is the producer
    /// generation it will accept and no other.
    pub fn new(launch_nonce: u128, runtime_generation: u64) -> Self {
        Self {
            launch_nonce,
            runtime_generation,
            surface_generation: 0,
            resource_epoch: 0,
            // Nothing is ready until the host says so. Starting at `true`
            // would make the very first generation the one case where a
            // producer could name a resource before the table existed.
            resources_ready: false,
            max_packet_bytes: MAX_TOTAL_BYTES,
            credits: Arc::new(CreditWindow::new(DEFAULT_MAX_CREDITS)),
            // One more buffer than the credit window: a packet is copied in
            // while the window's worth are still out with the renderer.
            pool: Arc::new(FramePool::new(
                DEFAULT_MAX_CREDITS as usize + 1,
                MAX_TOTAL_BYTES as usize,
            )),
            last_accepted_sequence: 0,
        }
    }

    /// Tighten the credit window. Clamped into `1..=MAX_CREDITS`: this cannot
    /// raise the ceiling, and zero would deadlock the producer rather than
    /// throttle it.
    pub fn with_max_credits(mut self, credits: u32) -> Self {
        let max = credits.clamp(1, MAX_CREDITS);
        self.credits = Arc::new(CreditWindow::new(max));
        self.pool = Arc::new(FramePool::new(
            max as usize + 1,
            self.max_packet_bytes as usize,
        ));
        self
    }

    /// Tighten this session's packet ceiling below [`MAX_TOTAL_BYTES`].
    ///
    /// Clamped, so a larger value leaves the absolute ceiling in place. A host
    /// on a memory-tight device can say something smaller; nothing can say
    /// something bigger, including a value that arrived from content.
    pub fn with_max_packet_bytes(mut self, bytes: u32) -> Self {
        self.max_packet_bytes = bytes.clamp(crate::HEADER_BYTES, MAX_TOTAL_BYTES);
        self.pool = Arc::new(FramePool::new(
            self.credits.max() as usize + 1,
            self.max_packet_bytes as usize,
        ));
        self
    }

    #[inline]
    pub const fn max_packet_bytes(&self) -> u32 {
        self.max_packet_bytes
    }

    /// The producer generation this ingress accepts, and no other.
    #[inline]
    pub const fn runtime_generation(&self) -> u64 {
        self.runtime_generation
    }

    #[inline]
    pub fn max_credits(&self) -> u32 {
        self.credits.max()
    }

    /// The buffer pool, for the memory ledger and the allocation gate.
    #[inline]
    pub fn pool(&self) -> &Arc<FramePool> {
        &self.pool
    }

    #[inline]
    pub const fn surface_generation(&self) -> u64 {
        self.surface_generation
    }

    #[inline]
    pub const fn resource_epoch(&self) -> u64 {
        self.resource_epoch
    }

    #[inline]
    pub const fn resources_ready(&self) -> bool {
        self.resources_ready
    }

    /// Advance to a new surface. Packets addressed to an older surface
    /// generation are rejected from here on: they were built against a size,
    /// scale or colour space that no longer describes what will be presented.
    ///
    /// Returns `false` and changes nothing if the value would move backwards.
    /// A timeline that can go back is a timeline on which a stale packet
    /// becomes valid again, and the caller that tried is the host, so the
    /// useful answer is a refusal it can assert on rather than a silent accept.
    #[must_use = "a refused timeline move means the caller's generation bookkeeping is wrong"]
    pub fn set_surface_generation(&mut self, generation: u64) -> bool {
        if generation < self.surface_generation {
            return false;
        }
        self.surface_generation = generation;
        true
    }

    /// Advance the resource epoch, invalidating every resource id a producer
    /// may still be holding. Used when the GPU context is lost and the resource
    /// table is rebuilt: ids are reused, so without an epoch a stale id from
    /// before the loss silently names a different object.
    ///
    /// Advancing clears the ready state. An epoch advance *means* the table was
    /// rebuilt, so nothing in it is ready yet by definition -- and the two
    /// facts belong to one call, because a host that had to remember to clear
    /// readiness separately is a host that will eventually forget.
    ///
    /// Returns `false` and changes nothing if the value would move backwards.
    #[must_use = "a refused timeline move means the caller's generation bookkeeping is wrong"]
    pub fn set_resource_epoch(&mut self, epoch: u64) -> bool {
        if epoch < self.resource_epoch {
            return false;
        }
        if epoch != self.resource_epoch {
            self.resource_epoch = epoch;
            self.resources_ready = false;
        }
        true
    }

    /// The host has verified every resource the current epoch admits, and
    /// packets may now name them.
    ///
    /// In v1 readiness is per-epoch. The per-resource, hash-verified form
    /// arrives with the resource protocol and can only narrow this rule: a
    /// packet that names an unready id will be rejected by a check that did not
    /// exist here, never accepted by one that was relaxed.
    pub fn mark_resources_ready(&mut self) {
        self.resources_ready = true;
    }

    #[inline]
    pub fn remaining_credits(&self) -> u32 {
        self.credits.remaining()
    }

    #[inline]
    pub fn in_flight(&self) -> u32 {
        self.credits.in_flight()
    }

    #[inline]
    pub const fn last_accepted_sequence(&self) -> u64 {
        self.last_accepted_sequence
    }

    /// Offer one packet.
    ///
    /// Every legality check runs before the credit check, and that order is
    /// deliberate. It costs a CRC pass on a packet that may be told to wait,
    /// and it buys the property that whether a packet is *legal* never depends
    /// on how busy the renderer is. The alternative answers `WouldBlock` to
    /// malformed bytes and invites the producer to resend them forever.
    pub fn submit(&mut self, bytes: &[u8]) -> (IngressOutcome, Option<PooledFrame>) {
        let (outcome, frame) = self.admit(bytes);
        if frame.is_some() {
            self.last_accepted_sequence = outcome.accepted_sequence;
        }
        (outcome, frame)
    }

    /// Commit sequence admission only after the consumer has validated and
    /// queued the frame. A refused frame can be retried with the same sequence.
    /// The consumer must drop its frame/credit on failure and return a nonzero
    /// error code. `&mut self` keeps admission and dispatch in the same order.
    pub fn submit_with(
        &mut self,
        bytes: &[u8],
        consume: impl FnOnce(PooledFrame) -> Result<(), u32>,
    ) -> IngressOutcome {
        let (mut outcome, frame) = self.admit(bytes);
        let Some(frame) = frame else {
            return outcome;
        };
        match consume(frame) {
            Ok(()) => {
                self.last_accepted_sequence = outcome.accepted_sequence;
                outcome.remaining_credits = self.remaining_credits();
                outcome
            }
            Err(code) => IngressOutcome::refused(code, self.remaining_credits()),
        }
    }

    fn admit(&mut self, bytes: &[u8]) -> (IngressOutcome, Option<PooledFrame>) {
        // The session ceiling first, before the parser walks anything: it is a
        // length comparison, and a packet above it is refused whatever else is
        // wrong with it.
        if bytes.len() > self.max_packet_bytes as usize {
            return (
                IngressOutcome::refused(INGRESS_ERROR_PACKET_TOO_LARGE, self.remaining_credits()),
                None,
            );
        }

        let frame = match validate(bytes) {
            Ok(frame) => frame,
            Err(error) => {
                return (
                    IngressOutcome::rejected(error, self.remaining_credits()),
                    None,
                );
            }
        };

        if frame.launch_nonce() != self.launch_nonce {
            return (
                IngressOutcome::refused(INGRESS_ERROR_FOREIGN_SESSION, self.remaining_credits()),
                None,
            );
        }

        // Generation before sequence: a packet from a dead generation is not a
        // sequencing mistake, and reporting it as one would send the producer
        // looking for a bug in its own counter.
        if frame.runtime_generation() != self.runtime_generation {
            return (
                IngressOutcome {
                    decision: IngressDecision::GenerationLost,
                    remaining_credits: self.remaining_credits(),
                    accepted_sequence: 0,
                    wire_error_code: 0,
                },
                None,
            );
        }

        if self.surface_generation != 0 && frame.surface_generation() != self.surface_generation {
            return (
                IngressOutcome::refused(INGRESS_ERROR_STALE_SURFACE, self.remaining_credits()),
                None,
            );
        }

        if frame.resource_epoch() != self.resource_epoch {
            return (
                IngressOutcome::refused(
                    INGRESS_ERROR_STALE_RESOURCE_EPOCH,
                    self.remaining_credits(),
                ),
                None,
            );
        }

        // Resource admission. A frame may only name ids the host has verified
        // for this epoch; before that, the resource table either does not exist
        // or has just been rebuilt, and an id in it names nothing or names the
        // wrong thing.
        if !self.resources_ready && frame.references_resources() {
            return (
                IngressOutcome::refused(
                    INGRESS_ERROR_RESOURCES_NOT_READY,
                    self.remaining_credits(),
                ),
                None,
            );
        }

        // Strictly contiguous, not merely increasing. A repeat is a replay, a
        // decrease is a reorder, and a *gap* means a packet carrying state was
        // lost -- all three would draw a frame twice, out of order, or missing
        // the state a previous packet established.
        //
        // Contiguity is affordable because a rejection is not recoverable in
        // the first place: contracts/apple/profile-policy.json answers
        // wire_validation_failed by terminating the content and voiding the
        // generation, so there is no "skip the bad one and carry on" path for a
        // gap to serve. A producer that is told to wait keeps its number,
        // because WouldBlock is decided after this check and consumes nothing.
        if frame.sequence() != self.last_accepted_sequence.saturating_add(1) {
            return (
                IngressOutcome::refused(
                    INGRESS_ERROR_NONCONTIGUOUS_SEQUENCE,
                    self.remaining_credits(),
                ),
                None,
            );
        }

        let sequence = frame.sequence();
        // The credit is taken before the copy, so a packet that cannot get one
        // is never copied. Copying first and releasing on failure would put a
        // packet-sized memcpy on the path a blocked producer retries.
        if !self.credits.try_acquire() {
            return (
                IngressOutcome {
                    decision: IngressDecision::WouldBlock,
                    remaining_credits: 0,
                    accepted_sequence: 0,
                    wire_error_code: 0,
                },
                None,
            );
        }

        // The bytes are borrowed from the caller for the duration of this call
        // only -- on Apple they point into a `Data` the Swift transport owns --
        // so nothing that outlives the call may reference them. One copy, into
        // a buffer this side owns and returns to the pool when the renderer is
        // finished with it.
        let Some(owned) = PooledFrame::new(bytes, &self.pool, &self.credits, sequence) else {
            // Only reachable if the pool refuses the length, which the session
            // ceiling already checked. Releasing here keeps the credit
            // accounting exact on a path that should not happen.
            drop(PooledFrame::new(&[], &self.pool, &self.credits, 0));
            return (
                IngressOutcome::refused(INGRESS_ERROR_PACKET_TOO_LARGE, self.remaining_credits()),
                None,
            );
        };

        (
            IngressOutcome {
                decision: IngressDecision::Accepted,
                remaining_credits: self.remaining_credits(),
                accepted_sequence: sequence,
                wire_error_code: 0,
            },
            Some(owned),
        )
    }
}
