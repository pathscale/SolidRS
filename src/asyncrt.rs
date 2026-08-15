//! Port of `src/core/async.ts`'s suspension machinery: `handleAsync`,
//! `asyncWrite`, `handleError`, and the autodispose release.
//!
//! The shape is upstream's, with the platform pieces substituted:
//!
//! | upstream | here |
//! |---|---|
//! | `result.then(onValue, onError)` | [`ErasedFlight::on_settle`] |
//! | `.then` firing synchronously | [`ErasedFlight::take_settled`] returning `Some` |
//! | `it.next()` | [`ErasedStream::take_buffered`] / `on_step` |
//! | the microtask that runs the continuation | [`crate::host::drive_microtasks`] |
//!
//! Nothing here polls or executes. A settle is a call the producer makes; the
//! only thing this module does is decide what the graph does about it.

use std::rc::Rc;

use crate::constants::{config, reactive, status};
use crate::error::ReactiveError;
use crate::graph::{trim_stale_deps, unobserved};
use crate::node::{AnyValue, ErasedFlight, ErasedStep, ErasedStream, NodeId};
use crate::runtime::{node, with_rt, Runtime};
use crate::scheduler::{init_transition, schedule};
use crate::status::{
    clear_status, notify_status, release_settled_dependents, set_pending_error,
    settle_pending_source,
};

/// The value `handle_async` settled on, or the suspension it raised.
pub(crate) enum AsyncOutcome {
    /// A value is available right now — a sync landing.
    Ready(AnyValue),
    /// The node is suspended. Upstream throws `NotReadyError` here.
    Suspended(ReactiveError),
    /// The flight had already failed.
    Failed(ReactiveError),
}

/// Port of `handleAsync` for the one-shot case.
pub(crate) fn handle_flight(
    rt: &Runtime,
    el: NodeId,
    flight: Rc<dyn ErasedFlight>,
) -> AsyncOutcome {
    let id = flight.flight_id();
    {
        let nodes = rt.nodes();
        node(&nodes, el).in_flight.set(id);
    }

    // A thenable that resolves synchronously never suspends: upstream's `isSync`
    // latch catches it and returns the value straight through, so no transition
    // opens and no boundary ever sees a pending.
    if let Some(settled) = flight.take_settled() {
        return match settled {
            Ok(v) => {
                let nodes = rt.nodes();
                let n = node(&nodes, el);
                n.in_flight.set(0);
                n.loading.set(false);
                AsyncOutcome::Ready(v)
            }
            Err(e) => {
                handle_error(rt, el, e.clone(), id);
                AsyncOutcome::Failed(e)
            }
        };
    }

    flight.on_settle(Box::new(move |outcome| {
        with_rt(|rt| match outcome {
            Ok(v) => {
                async_write(rt, el, v, id);
                settle_autodispose(rt, el);
            }
            Err(e) => {
                handle_error(rt, el, e, id);
                settle_autodispose(rt, el);
            }
        });
    }));

    suspend(rt, el)
}

/// Port of `handleAsync`'s iterator branch.
pub(crate) fn handle_stream(
    rt: &Runtime,
    el: NodeId,
    stream: Rc<dyn ErasedStream>,
) -> AsyncOutcome {
    let id = stream.stream_id();
    {
        let nodes = rt.nodes();
        let n = node(&nodes, el);
        // Close any previous stream before adopting the new one: the old
        // producer must stop pulling. Port of the iterator `return()` upstream
        // registers in `cleanup`.
        if let Some(prev) = n.in_flight_stream.borrow_mut().take() {
            prev.close();
        }
        n.in_flight.set(id);
        *n.in_flight_stream.borrow_mut() = Some(stream.clone());
    }

    // Drain whatever the producer already buffered — upstream's synchronous
    // `iterate()` loop, which keeps pulling while steps settle synchronously.
    let mut sync_value: Option<AnyValue> = None;
    loop {
        match stream.take_buffered() {
            Some(ErasedStep::Yield(v)) => sync_value = Some(v),
            Some(ErasedStep::Error(e)) => {
                handle_error(rt, el, e.clone(), id);
                return AsyncOutcome::Failed(e);
            }
            Some(ErasedStep::Done) => break,
            None => break,
        }
    }

    let stream_for_cb = stream.clone();
    stream.on_step(Box::new(move |step| {
        with_rt(|rt| {
            let current = {
                let nodes = rt.nodes();
                nodes.get(el).map(|n| n.in_flight.get()).unwrap_or(0)
            };
            if current != id {
                stream_for_cb.close();
                return;
            }
            match step {
                ErasedStep::Yield(v) => {
                    async_write(rt, el, v, id);
                    // An unobserved lazy node must tear down instead of pumping
                    // the stream forever with zero subscribers.
                    if settle_autodispose(rt, el) {
                        stream_for_cb.close();
                    }
                }
                ErasedStep::Error(e) => {
                    handle_error(rt, el, e, id);
                    settle_autodispose(rt, el);
                }
                ErasedStep::Done => {
                    let nodes = rt.nodes();
                    if let Some(n) = nodes.get(el) {
                        n.in_flight.set(0);
                    }
                    drop(nodes);
                    settle_autodispose(rt, el);
                }
            }
        });
    }));

    match sync_value {
        // A sync first yield is the first real answer.
        Some(v) => {
            let nodes = rt.nodes();
            node(&nodes, el).loading.set(false);
            AsyncOutcome::Ready(v)
        }
        None => suspend(rt, el),
    }
}

/// The suspension point: no value yet.
fn suspend(rt: &Runtime, el: NodeId) -> AsyncOutcome {
    // Commit-#0 loading window (A27): serve the declared value instead of
    // suspending. No transition opens — first-flight work on a `loading_value`
    // node is loading-class, invisible to boundaries and transitions.
    let (loading, committed) = {
        let nodes = rt.nodes();
        let n = node(&nodes, el);
        let out = (n.loading.get(), n.value.borrow().clone());
        out
    };
    if loading {
        if let Some(v) = committed {
            return AsyncOutcome::Ready(v);
        }
    }
    init_transition(rt, resolve_transition(rt, el));
    AsyncOutcome::Suspended(ReactiveError::not_ready(Some(el)))
}

/// Port of `resolveTransition` minus the lane and override arms, which belong
/// to `lanes.ts` / `optimistic.ts`.
pub(crate) fn resolve_transition(rt: &Runtime, el: NodeId) -> Option<crate::node::TransitionId> {
    let nodes = rt.nodes();
    nodes.get(el).and_then(|n| n.transition.get())
}

/// Port of `asyncWrite`.
pub(crate) fn async_write(rt: &Runtime, el: NodeId, value: AnyValue, flight: usize) {
    {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return };
        // Superseded: a newer computation produced a different flight.
        if n.in_flight.get() != flight {
            return;
        }
        // Dirtied by a newer write — the upcoming flush recomputes the node and
        // creates a fresh flight that supersedes this one.
        if n.has_flag(reactive::DIRTY | reactive::OPTIMISTIC_DIRTY) {
            return;
        }
    }

    // NB: `in_flight` is deliberately *not* cleared here. It is the identity a
    // later step compares against, and a stream keeps landing through it;
    // `recompute` clears it when it replaces the flight. Clearing it on the
    // first landing closed every stream after one yield.
    settle_transition(rt, el);
    trim_stale_deps(rt, el);
    clear_status(rt, el, false);

    // The plain landing path. Upstream's setter / override / lane branches
    // belong to the projection, optimistic and lane ports.
    crate::core::set_signal(rt, el, move |_| value);

    {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return };
        // The window closes when the answer becomes *observable*: a
        // transition-held write is not, and `commit_pending_node` closes it
        // when the hold commits.
        if n.pending_value.borrow().is_none() {
            n.loading.set(false);
        }
        n.time.set(rt.clock.get());
    }

    settle_pending_source(rt, el);
    schedule(rt);
    crate::scheduler::flush();
}

/// Port of `handleError`.
pub(crate) fn handle_error(rt: &Runtime, el: NodeId, error: ReactiveError, flight: usize) {
    {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return };
        if n.in_flight.get() != flight {
            return;
        }
    }

    let still_pending = error.is_not_ready();

    // Loading window: the flight died waiting on an unready source. Keep
    // serving commit #0 — the same parking `recompute` does for a sync
    // dependency throw.
    let loading = {
        let nodes = rt.nodes();
        nodes.get(el).map(|n| n.loading.get()).unwrap_or(false)
    };
    if still_pending && loading {
        park_loading_window(rt, el, &error);
        let nodes = rt.nodes();
        if let Some(n) = nodes.get(el) {
            n.time.set(rt.clock.get());
        }
        return;
    }

    settle_transition(rt, el);
    notify_status(
        rt,
        el,
        if still_pending { status::PENDING } else { status::ERROR },
        Some(error),
        false,
    );
    {
        let nodes = rt.nodes();
        if let Some(n) = nodes.get(el) {
            n.time.set(rt.clock.get());
        }
    }
    // A real error settles derivatively-pending dependents, so stranded lazy
    // ones release here — the error twin of `settle_pending_source`'s release.
    if !still_pending {
        release_settled_dependents(rt, el);
    }
    schedule(rt);
    crate::scheduler::flush();
}

/// Port of `parkLoadingWindow`.
pub(crate) fn park_loading_window(rt: &Runtime, el: NodeId, e: &ReactiveError) {
    {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return };
        n.blocked.set(true);
    }
    if let Some(src) = e.not_ready_source() {
        crate::status::add_pending_source(rt, el, src);
    }
    // A settled error is the node's answer; the park must not replace it.
    let errored = {
        let nodes = rt.nodes();
        nodes.get(el).map(|n| n.has_status(status::ERROR)).unwrap_or(false)
    };
    if !errored {
        set_pending_error(rt, el, e.not_ready_source(), Some(e.clone()));
    }
}

/// Port of the settle-time transition re-entry in `handleAsync`.
///
/// The loading rail is invisible to transactions: a boundary-caught first load
/// never registers as an async reporter, so its settle must flow ambiently.
/// Blindly re-entering a stamped, still-incomplete transaction stashed the
/// reveal with it — a deadlock when that transaction's completion depended on
/// the reveal (upstream #2937).
fn settle_transition(rt: &Runtime, el: NodeId) {
    let transition = resolve_transition(rt, el);
    if let Some(t) = transition {
        let uninitialized = {
            let nodes = rt.nodes();
            nodes
                .get(el)
                .map(|n| n.has_status(status::UNINITIALIZED))
                .unwrap_or(false)
        };
        let is_reporter = {
            let live = crate::scheduler::current_transition(rt, t);
            let ts = rt.transitions.borrow();
            ts.get(live)
                .map(|tr| tr.async_reporters.borrow().values().any(|s| s.contains(&el)))
                .unwrap_or(false)
        };
        if uninitialized && !is_reporter {
            // Drop the stale stamp too: the plain settle write re-enters the
            // transaction through it.
            let nodes = rt.nodes();
            if let Some(n) = nodes.get(el) {
                n.transition.set(None);
            }
            return;
        }
    }
    init_transition(rt, transition);
}

/// Port of `settleAutodispose`.
///
/// A pending node's in-flight work is an observer — `unlink_subs` skips
/// autodispose while `STATUS_PENDING`, so subscriber churn cannot orphan it.
/// Settling is that observer's release, so it runs the same last-one-out check
/// every other release site runs. Returns whether the node released.
pub(crate) fn settle_autodispose(rt: &Runtime, el: NodeId) -> bool {
    let release = {
        let nodes = rt.nodes();
        match nodes.get(el) {
            Some(n) => {
                n.has_config(config::AUTO_DISPOSE)
                    && n.subs.get().is_none()
                    && !n.has_status(status::PENDING)
            }
            None => false,
        }
    };
    if release {
        unobserved(rt, el);
    }
    release
}
