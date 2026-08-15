//! Port of `src/core/constants.ts`.
//!
//! The bit layout is reproduced exactly, including bit positions, so that a
//! flag dump from a Rust debug session can be read against the TypeScript
//! source (and against `docs/BITWISE_OPERATIONS.md`) without a translation
//! table.

/// Scheduler / lifecycle bits stored in `Node::flags`.
pub mod reactive {
    #[allow(dead_code, reason = "named for parity with the TS constant table")]
    pub const NONE: u32 = 0;
    pub const CHECK: u32 = 1 << 0;
    pub const DIRTY: u32 = 1 << 1;
    pub const RECOMPUTING_DEPS: u32 = 1 << 2;
    pub const IN_HEAP: u32 = 1 << 3;
    pub const IN_HEAP_HEIGHT: u32 = 1 << 4;
    pub const ZOMBIE: u32 = 1 << 5;
    pub const DISPOSED: u32 = 1 << 6;
    pub const OPTIMISTIC_DIRTY: u32 = 1 << 7;
    pub const SNAPSHOT_STALE: u32 = 1 << 8;
    pub const LAZY: u32 = 1 << 9;
    pub const MANUAL_WRITE: u32 = 1 << 10;
    /// The pending recompute is a re-ask of the same question: `refresh()`
    /// dirtied the node while no tracked input changed value. Cleared whenever
    /// a real value-change notification arrives (`insert_subs`), and consumed
    /// by `recompute` into the node's re-ask classification.
    pub const REASK: u32 = 1 << 11;
}

/// Static per-node configuration stored in `Node::config`.
pub mod config {
    pub const OWNED_WRITE: u32 = 1 << 0;
    /// Snapshot capture is a separate port; the bit is reserved so the
    /// layout stays comparable with `constants.ts`.
    #[allow(dead_code, reason = "snapshot capture is out of scope")]
    pub const NO_SNAPSHOT: u32 = 1 << 1;
    pub const TRANSPARENT: u32 = 1 << 2;
    pub const IN_SNAPSHOT_SCOPE: u32 = 1 << 3;
    /// Set by `create_tracked_effect` after creation. That primitive is a
    /// later port, so nothing sets the bit yet; it is reserved so the layout
    /// stays comparable with `constants.ts`.
    #[allow(dead_code, reason = "set by createTrackedEffect, a later port")]
    pub const CHILDREN_FORBIDDEN: u32 = 1 << 4;
    pub const AUTO_DISPOSE: u32 = 1 << 5;
    pub const SYNC: u32 = 1 << 6;
}

/// Async / error status stored in `Node::status_flags`.
pub mod status {
    #[allow(dead_code, reason = "named for parity with the TS constant table")]
    pub const NONE: u32 = 0;
    pub const PENDING: u32 = 1 << 0;
    pub const ERROR: u32 = 1 << 1;
    pub const UNINITIALIZED: u32 = 1 << 2;
}

/// Effect kind. Upstream stores this in `_type`, where the *absence* of the
/// field means "not an effect" and `EFFECT_PURE = 0` is never used as a node
/// type. We model that honestly with `Option<EffectKind>` on the node, so the
/// upstream truthiness test `(el as any)._type` becomes `kind.is_some()`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum EffectKind {
    Render = 1,
    User = 2,
    Tracked = 3,
}

impl EffectKind {
    /// The queue index an effect of this kind is enqueued into.
    ///
    /// Mirrors upstream's `_queues[type - 1]`: index 0 is render, index 1 is
    /// user. Tracked effects enqueue as user callbacks.
    pub const fn queue_slot(self) -> usize {
        match self {
            EffectKind::Render => 0,
            EffectKind::User | EffectKind::Tracked => 1,
        }
    }

    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}
