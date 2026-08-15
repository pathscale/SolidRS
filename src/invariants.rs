//! Port of `src/core/invariants.ts`.
//!
//! These are implementation self-consistency checks, not semantic rules: a
//! violation means the reactive system contradicted itself. The port brief
//! calls them the spec, and they are ported as debug assertions.
//!
//! Gating matches upstream: the checks live behind `test_invariants` (their
//! `__TEST__`), not `dev` (their `__DEV__`), because the per-write tracking
//! sets and the per-flush quiescence sweep are too expensive for a shipped dev
//! build. Call sites stay `cfg`-guarded so release builds compile them out
//! entirely.
//!
//! Which invariants are live here:
//!
//! | id     | subject                                   | status                        |
//! |--------|-------------------------------------------|-------------------------------|
//! | INV-1  | `isPending` probe leak at flush start      | live (probe never set: trivially holds) |
//! | INV-7  | held `pending_value` at quiescence         | **live and load-bearing**     |
//! | INV-2  | active optimistic override registered      | inert — `optimistic.ts` out of scope |
//! | INV-3  | async reporters written only from notify   | inert — no async source exists |
//! | INV-4  | companion coherence                        | inert — verdict layer out of scope |
//! | INV-5  | merged lane empty                          | inert — `lanes.ts` out of scope |
//! | INV-6  | override reverted by quiescence            | inert — as INV-2              |
//! | INV-9  | companion false for a disposed owner       | inert — as INV-4              |
//! | INV-10 | `affects()` mark released                  | inert — `affects.ts` out of scope |
//!
//! The inert ones are listed rather than deleted so the later ports know which
//! check they are expected to re-arm.

#![cfg(feature = "test_invariants")]

use std::cell::RefCell;
use std::collections::HashSet;

use crate::constants::reactive;
use crate::node::NodeId;
use crate::runtime::Runtime;

thread_local! {
    /// INV-7: nodes that received a transition-held `pending_value`. A node
    /// still holding one at quiescence with no queued commit is a leak.
    static HELD_PENDING: RefCell<HashSet<NodeId>> = RefCell::new(HashSet::new());
}

fn assert_invariant(ok: bool, id: &str, message: &str) {
    assert!(ok, "solid_rs invariant {id} violated: {message}");
}

/// Record that `id` staged a value.
pub(crate) fn track_held_pending(id: NodeId) {
    HELD_PENDING.with(|s| {
        s.borrow_mut().insert(id);
    });
}

/// INV-1: an `isPending()` probe must never leak past its own call.
pub(crate) fn dev_check_flush_start(rt: &Runtime) {
    assert_invariant(
        !rt.pending_check_active.get(),
        "INV-1",
        "pending probe is set at flush start — an isPending() probe leaked",
    );
    assert_invariant(
        !rt.latest_read_active.get(),
        "INV-1",
        "latest() read window is open at flush start",
    );
}

/// Quiescence checks. Run only when the system is fully drained: nothing
/// scheduled, no transitions. At that point no transaction-scoped state may
/// survive.
pub(crate) fn dev_check_quiescent(rt: &Runtime) {
    let queued: HashSet<NodeId> = {
        let ts = rt.transitions.borrow();
        ts.get(rt.batch.get())
            .map(|b| b.pending_nodes.borrow().iter().copied().collect())
            .unwrap_or_default()
    };

    HELD_PENDING.with(|s| {
        let mut s = s.borrow_mut();
        s.retain(|&id| {
            let nodes = rt.nodes();
            let Some(n) = nodes.get(id) else { return false };
            if n.has_flag(reactive::DISPOSED) || n.pending_value.borrow().is_none() {
                return false;
            }
            // Queued nodes commit on the next flush; everything else is
            // unreachable.
            assert_invariant(
                queued.contains(&id),
                "INV-7",
                "a node holds a staged value at quiescence with no queued commit and no \
                 transition — the value can never commit",
            );
            true
        });
    });
}
