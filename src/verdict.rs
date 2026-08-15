//! Port of `src/core/verdict.ts`: the `is_pending()` / `latest()` layer.
//!
//! The definition is A19/A24, question-scoped: **a read is pending iff a value
//! change is in flight for it that has not yet revealed.** Three causes, each
//! ending on its own terms — a write held by a live transition (ends at
//! commit), the node's own async in flight (ends at resolution), a fresh value
//! held uncommitted by an entangled transition (ends at that commit) — and one
//! filter: work whose tracked inputs are value-stable is a *re-ask of the same
//! question*, which is quiet, because the shown answer still answers it.
//!
//! Two exceptions carry over verbatim: an uninitialized source is *loading*,
//! not pending (no observable value exists to be non-final), and a commit-#0
//! loading window is quiet by declaration (A27).
//!
//! Companions are lazy and probe-created, exactly as upstream: `pending_signal`
//! is a boolean signal, `latest_value` a shadow computed. Every write path
//! feeds them through [`sync_companions`], which is the single chokepoint the
//! #2831 fix established.

use std::rc::Rc;

use crate::constants::{config, reactive, status};
use crate::core::{create_signal_node, prepare_computed, read_node, set_signal, ReadOutcome, SignalSpec};
use crate::node::{AnyValue, NodeId};
use crate::runtime::{node, with_rt, Runtime};

thread_local! {
    /// The active `is_pending()` probe: the sources its thunk read.
    static PROBE: std::cell::RefCell<Option<Probe>> = const { std::cell::RefCell::new(None) };
}

#[derive(Default)]
struct Probe {
    sources: Vec<NodeId>,
    /// Sources whose *fresh* (staged) value this read observed. A reader that
    /// saw the new value must not also be told it is pending — the A10 pair
    /// rule.
    fresh_reads: Vec<NodeId>,
}

/// Port of `quietPending`: is every source blocking this node a re-ask of its
/// own unchanged question?
fn quiet_pending(rt: &Runtime, el: NodeId) -> bool {
    let nodes = rt.nodes();
    let Some(n) = nodes.get(el) else { return false };
    let sources = n.pending_sources.borrow();
    match &*sources {
        Some(set) => {
            for src in set.iter() {
                let quiet = nodes.get(*src).map(|s| s.reask.get()).unwrap_or(false);
                if !quiet {
                    return false;
                }
            }
            true
        }
        None => n.reask.get(),
    }
}

/// Port of `newQuestionInFlight`.
///
/// An open commit-#0 loading window is deliberately absent from this test:
/// commit #0 answers the question by declaration, so the window reads *not*
/// pending and first-load affordances live in the value channel (A27).
fn new_question_in_flight(rt: &Runtime, el: NodeId) -> bool {
    let (pending, uninit) = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return false };
        (n.has_status(status::PENDING), n.has_status(status::UNINITIALIZED))
    };
    pending && !uninit && !quiet_pending(rt, el)
}

/// Port of `computePendingState` — the verdict ladder.
pub(crate) fn compute_pending_state(rt: &Runtime, el: NodeId) -> bool {
    let (disposed, parent_source, held, uninit, loading) = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return false };
        let out = (
            n.has_flag(reactive::DISPOSED),
            n.parent_source.get(),
            n.pending_value.borrow().is_some(),
            n.has_status(status::UNINITIALIZED),
            n.loading.get(),
        );
        out
    };

    // A dead source can never settle; a latched `true` would hold a spinner
    // forever (INV-9).
    if disposed {
        return false;
    }

    // Latest-shadow branch: the fresh channel. The shadow already shows held
    // values, so a hold cannot supersede what it shows — only actually
    // in-flight async counts (A8).
    if let Some(parent) = parent_source {
        return new_question_in_flight(rt, parent);
    }

    // Held value: a pending commit supersedes the committed value (A19 causes
    // i and iii). A hold created while the loading window is still open is the
    // window's own landing in flight to its commit — quiet like the rest of the
    // window (upstream #2990).
    if held && !uninit && !loading {
        return true;
    }

    new_question_in_flight(rt, el)
}

// ------------------------------------------------------------- companions

/// Port of `getPendingSignal`. Lazy: an app that never asks pays nothing.
fn pending_signal_of(rt: &Runtime, el: NodeId) -> NodeId {
    if let Some(existing) = {
        let nodes = rt.nodes();
        nodes.get(el).and_then(|n| n.pending_signal.get())
    } {
        return existing;
    }

    // Starts false and is written true if pending, so a reversion returns to
    // false rather than latching.
    let sig = create_signal_node(
        rt,
        SignalSpec {
            value: Some(Rc::new(false) as AnyValue),
            equals: Some(Rc::new(|a: &dyn std::any::Any, b: &dyn std::any::Any| {
                a.downcast_ref::<bool>() == b.downcast_ref::<bool>()
            })),
            unobserved: None,
            owned_write: true,
            name: "isPending",
        },
    );
    {
        let nodes = rt.nodes();
        node(&nodes, sig).parent_source.set(Some(el));
        node(&nodes, el).pending_signal.set(Some(sig));
    }
    if compute_pending_state(rt, el) {
        set_signal(rt, sig, |_| Rc::new(true) as AnyValue);
    }
    sig
}

/// Port of `syncCompanions` — the single companion-update chokepoint every
/// write path goes through.
pub(crate) fn sync_companions(rt: &Runtime, el: NodeId, value: &AnyValue) {
    let (sig, shadow) = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return };
        (n.pending_signal.get(), n.latest_value.get())
    };
    if sig.is_some() {
        update_pending_signal(rt, el);
    }
    if let Some(shadow) = shadow {
        let v = value.clone();
        set_signal(rt, shadow, move |_| v);
    }
}

/// Port of `updatePendingSignal`.
pub(crate) fn update_pending_signal(rt: &Runtime, el: NodeId) {
    let (sig, shadow) = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return };
        (n.pending_signal.get(), n.latest_value.get())
    };
    if let Some(sig) = sig {
        let pending = compute_pending_state(rt, el);
        set_signal(rt, sig, move |_| Rc::new(pending) as AnyValue);
    }
    if let Some(shadow) = shadow {
        update_pending_signal(rt, shadow);
    }
}

/// Port of `snapCompanionsToState`: re-derive a companion and write the verdict
/// **committed**, not through `set_signal`.
///
/// This runs inside queue finalization, where a staged write would open a fresh
/// window that nothing settles — rescheduling forever while async is in flight.
pub(crate) fn snap_companions(rt: &Runtime, owner: NodeId) {
    let sig = {
        let nodes = rt.nodes();
        nodes.get(owner).and_then(|n| n.pending_signal.get())
    };
    if let Some(sig) = sig {
        let pending = compute_pending_state(rt, owner);
        let changed = {
            let nodes = rt.nodes();
            let Some(s) = nodes.get(sig) else { return };
            let current = s
                .value
                .borrow()
                .as_ref()
                .and_then(|v| v.downcast_ref::<bool>().copied())
                .unwrap_or(false);
            let staged = s.pending_value.borrow().is_some();
            if current != pending || staged {
                *s.value.borrow_mut() = Some(Rc::new(pending) as AnyValue);
                *s.pending_value.borrow_mut() = None;
                s.time.set(rt.clock.get());
                true
            } else {
                false
            }
        };
        if changed {
            crate::scheduler::insert_subs(rt, sig, false);
            crate::scheduler::schedule(rt);
        }
    }
}

// ------------------------------------------------------------ public API

/// Read `f`'s sources and report whether any of them is pending.
///
/// "Pending" is question-scoped: a value change in flight that has not revealed
/// yet. A re-ask of the same question — `refresh()`, a poll, an action's confirm
/// refetch — is quiet, because the shown value still answers the question.
///
/// The probe is **reads-only**: the thunk's return value is never inspected
/// (A23). Ask whole-object questions through reads.
///
/// ```
/// use solid_rs::*;
/// let (flight, resolver) = Flight::<i32>::new();
/// let (tick, set_tick) = create_signal(0);
/// let data = create_root(move |_| {
///     let f = flight.clone();
///     create_async_memo(move |_| { tick.get(); Async::Flight(f.clone()) })
/// });
/// create_root(move |_| {
///     create_render_effect(move |_| data.try_get().ok(), move |_v: &Option<i32>, _| {});
/// });
/// flush();
/// // Uninitialized is *loading*, not pending.
/// assert!(!is_pending(move || { let _ = data.try_get(); }));
/// resolver.resolve(1);
/// host::drive_microtasks();
/// assert_eq!(data.get(), 1);
/// ```
pub fn is_pending(f: impl FnOnce()) -> bool {
    with_rt(|rt| {
        let prev_active = rt.pending_check_active.get();
        let prev_probe = PROBE.with(|p| p.borrow_mut().take());
        rt.pending_check_active.set(true);
        PROBE.with(|p| *p.borrow_mut() = Some(Probe::default()));

        f();

        rt.pending_check_active.set(false);
        let probe = PROBE.with(|p| p.borrow_mut().take()).unwrap_or_default();

        let mut found = false;
        for src in &probe.sources {
            if probe.fresh_reads.contains(src) {
                // The A10 pair rule: a reader that observed the fresh value is
                // not also told it is pending.
                continue;
            }
            let sig = pending_signal_of(rt, *src);
            let verdict = match read_node(rt, sig) {
                ReadOutcome::Value => crate::core::current_value(rt, sig)
                    .and_then(|v| v.downcast_ref::<bool>().copied())
                    .unwrap_or(false),
                ReadOutcome::Err(_) => false,
            };
            if verdict {
                found = true;
            }
        }

        rt.pending_check_active.set(prev_active);
        PROBE.with(|p| *p.borrow_mut() = prev_probe);
        found
    })
}

/// Record a node the probe's thunk read. Called from `read_node`.
pub(crate) fn collect_probe_source(el: NodeId, fresh: bool) {
    PROBE.with(|p| {
        if let Some(probe) = &mut *p.borrow_mut() {
            if !probe.sources.contains(&el) {
                probe.sources.push(el);
            }
            if fresh && !probe.fresh_reads.contains(&el) {
                probe.fresh_reads.push(el);
            }
        }
    });
}

/// Prepare a node during a probe read without letting the probe's own link
/// become a value dependency. Port of `pendingCheckRead`.
pub(crate) fn probe_prepare(rt: &Runtime, el: NodeId) {
    let is_computed = {
        let nodes = rt.nodes();
        nodes.get(el).map(|n| n.is_computed()).unwrap_or(false)
    };
    if is_computed {
        rt.pending_check_active.set(false);
        prepare_computed(rt, el, true);
        rt.pending_check_active.set(true);
    }
}

/// True when an `is_pending` probe is collecting. Read by the boundary layer.
#[allow(dead_code, reason = "consumed by boundaries.ts, a later port")]
pub(crate) fn probe_active() -> bool {
    PROBE.with(|p| p.borrow().is_some())
}

/// Read the freshest value a source has, ignoring transition holds.
///
/// `latest` is an override the system writes for itself the moment a held value
/// exists (A8), so it shows the in-flight value while the committed channel
/// still shows the old one.
pub fn latest<T>(f: impl FnOnce() -> T) -> T {
    with_rt(|rt| {
        let prev = rt.latest_read_active.get();
        rt.latest_read_active.set(true);
        let out = f();
        rt.latest_read_active.set(prev);
        out
    })
}

/// The `latest()`-mode read: prefer a staged value over the committed one.
pub(crate) fn latest_value_of(rt: &Runtime, el: NodeId) -> Option<AnyValue> {
    let nodes = rt.nodes();
    let n = nodes.get(el)?;
    let staged = n.pending_value.borrow().clone();
    let out = staged.or_else(|| n.value.borrow().clone());
    out
}

/// Port of `applyReask`: consume the `REASK` flag into the node's classification
/// with the monotone guard — a node already pending on an unanswered *new*
/// question stays non-quiet.
pub(crate) fn apply_reask(rt: &Runtime, el: NodeId, had_reask: bool) -> bool {
    let nodes = rt.nodes();
    let Some(n) = nodes.get(el) else { return false };
    let was_pending = n.has_status(status::PENDING);
    let is_reask = had_reask && !(was_pending && !n.reask.get());
    let changed = was_pending && n.reask.get() != is_reask;
    n.reask.set(is_reask);
    changed
}

/// Whether a node still counts as an autodispose candidate while pending.
#[allow(dead_code)]
pub(crate) fn exempt_from_autodispose(rt: &Runtime, el: NodeId) -> bool {
    let nodes = rt.nodes();
    nodes
        .get(el)
        .map(|n| n.has_status(status::PENDING) && n.has_config(config::AUTO_DISPOSE))
        .unwrap_or(false)
}
