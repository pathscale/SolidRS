//! Port of `src/core/effect.ts`.
//!
//! Effects are the leaf nodes of the reactive graph, and the only place where
//! the two phases are visibly separate: `compute` runs tracked inside the
//! graph, `effect` runs untracked on the queue after the graph has settled.
//! That split is why reactive reads inside the apply phase do not re-trigger
//! the effect — deliberate, not a limitation.

use std::cell::RefCell;
use std::rc::Rc;

use crate::constants::{config, reactive, status, EffectKind};
use crate::core::{create_computed, recompute, ComputedSpec};
use crate::error::ReactiveError;
use crate::node::{Disposable, EffectFn, ErrorFn, NodeId};
use crate::runtime::{node, with_rt, Runtime};
use crate::scheduler::{halt_reactivity, queue_enqueue, queue_notify};

/// Build the cached queue callback for an effect node.
pub(crate) fn make_runner(id: NodeId) -> Rc<dyn Fn(u8)> {
    Rc::new(move |_ty| {
        with_rt(|rt| run_effect(rt, id));
    })
}

pub(crate) struct EffectSpec {
    pub spec: ComputedSpec,
    pub effect_fn: EffectFn,
    pub error_fn: Option<ErrorFn>,
    pub defer: bool,
    pub schedule: bool,
}

/// Port of `effect()`.
pub(crate) fn create_effect_node(rt: &Runtime, mut es: EffectSpec) -> NodeId {
    let kind = es.spec.effect_kind.expect("effect nodes carry a kind");
    es.spec.lazy = true;
    let id = create_computed(rt, es.spec);

    {
        let nodes = rt.nodes();
        let n = node(&nodes, id);
        *n.effect_fn.borrow_mut() = Some(Rc::new(RefCell::new(es.effect_fn)));
        *n.error_fn.borrow_mut() = es.error_fn.map(|f| Rc::new(RefCell::new(f)));
        *n.bound_run_effect.borrow_mut() = Some(make_runner(id));
        // Effects live for their owner's lifetime rather than being torn down
        // when their subscriber count hits zero.
        n.config.set(n.config.get() & !config::AUTO_DISPOSE);
        *n.notify_status.borrow_mut() = Some(make_status_notifier(id));
    }

    recompute(rt, id, true);

    if !es.defer {
        if kind == EffectKind::User || es.schedule {
            let (queue, runner) = {
                let nodes = rt.nodes();
                let n = node(&nodes, id);
                let out = (n.queue.get(), n.bound_run_effect.borrow().clone());
                out
            };
            if let Some(r) = runner {
                queue_enqueue(rt, queue, kind, r);
            }
        } else {
            run_effect(rt, id);
        }
    }

    #[cfg(feature = "dev")]
    {
        let no_owner = {
            let nodes = rt.nodes();
            node(&nodes, id).parent.get().is_none()
        };
        if no_owner {
            eprintln!(
                "[NO_OWNER_EFFECT] Effects created outside a reactive context will never be \
                 disposed"
            );
        }
    }

    id
}

/// Port of `notifyEffectStatus`.
fn make_status_notifier(id: NodeId) -> Rc<dyn Fn(Option<u32>, Option<ReactiveError>)> {
    Rc::new(move |st, err| {
        with_rt(|rt| notify_effect_status(rt, id, st, err));
    })
}

fn notify_effect_status(
    rt: &Runtime,
    id: NodeId,
    st: Option<u32>,
    err: Option<ReactiveError>,
) {
    let (actual_status, actual_error, kind, queue) = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(id) else { return };
        (
            st.unwrap_or_else(|| n.status_flags.get()),
            err.or_else(|| n.error.borrow().clone()),
            n.effect_kind.get(),
            n.queue.get(),
        )
    };

    if actual_status & status::ERROR != 0 {
        queue_notify(rt, queue, id, status::PENDING, 0, None);

        if kind == Some(EffectKind::User) {
            // The error handler is the error arm of the effect phase: queue it
            // like the effect function. It runs in the same imperative,
            // writable scope, and escalates the same way. No payload is
            // queued — the node already carries its status and error, and the
            // runner dispatches on them, so a recovery before the effect phase
            // takes the success arm instead.
            let still_errored = {
                let nodes = rt.nodes();
                node(&nodes, id).has_status(status::ERROR)
            };
            if still_errored {
                let runner = {
                    let nodes = rt.nodes();
                    let n = node(&nodes, id);
                    n.modified.set(true);
                    let out = n.bound_run_effect.borrow().clone();
                    out
                };
                if let Some(r) = runner {
                    queue_enqueue(rt, queue, EffectKind::User, r);
                }
            }
            return;
        }

        if !queue_notify(rt, queue, id, status::ERROR, status::ERROR, actual_error.clone()) {
            halt_reactivity(rt, actual_error.as_ref());
            panic_with(actual_error);
        }
    } else if kind == Some(EffectKind::Render) {
        queue_notify(
            rt,
            queue,
            id,
            status::PENDING | status::ERROR,
            actual_status,
            actual_error,
        );
    }
}

/// An error that escaped every boundary. Upstream rethrows here, unwinding the
/// flush; the Rust equivalent is a panic, and the system is already halted so
/// no further updates are processed.
fn panic_with(err: Option<ReactiveError>) -> ! {
    match err {
        Some(e) => panic!("solid_rs: uncaught reactive error: {}", e.message()),
        None => panic!("solid_rs: uncaught reactive error"),
    }
}

/// Port of `runEffect`.
pub(crate) fn run_effect(rt: &Runtime, id: NodeId) {
    let (modified, disposed, errored, kind) = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(id) else { return };
        (
            n.modified.get(),
            n.has_flag(reactive::DISPOSED),
            n.has_status(status::ERROR),
            n.effect_kind.get(),
        )
    };
    if !modified || disposed {
        return;
    }

    // Error arm, user effects only: a compute-phase error that is still the
    // node's settled state at effect time runs the bundle's error handler in
    // this same imperative, writable scope. Render effects bypass — their
    // errors route to boundaries synchronously in the status notifier, and a
    // runner queued by an earlier value change in the same flush must not be
    // hijacked by a later-arriving error status.
    if errored && kind == Some(EffectKind::User) {
        let (err, handler) = {
            let nodes = rt.nodes();
            let n = node(&nodes, id);
            let raw = n.error.borrow().clone();
            *n.prev_value.borrow_mut() = n.value.borrow().clone();
            n.modified.set(false);
            let out = (raw, n.error_fn.borrow().clone());
            out
        };

        let user_err = err.as_ref().and_then(|e| e.unwrap_status());
        match (handler, user_err) {
            (Some(h), Some(payload)) => {
                let run_cleanup = || {
                    let taken = with_rt(|rt| {
                        let nodes = rt.nodes();
                        nodes.get(id).and_then(|n| n.cleanup.borrow_mut().take())
                    });
                    if let Some(c) = taken {
                        c();
                    }
                };
                let mut h = h.borrow_mut();
                h(payload, &run_cleanup);
            }
            _ => {
                // No handler: log and keep the system alive — the run was
                // skipped, but a non-render effect's reactivity failing does
                // not crash the app.
                if let Some(e) = &err {
                    eprintln!("solid_rs: unhandled effect compute error: {}", e.message());
                }
            }
        }
        return;
    }

    // Clone the values out so the apply phase runs with no arena borrow held.
    let (value, prev_value, effect_fn, prev_cleanup) = {
        let nodes = rt.nodes();
        let n = node(&nodes, id);
        let out = (
            n.value.borrow().clone(),
            n.prev_value.borrow().clone(),
            n.effect_fn.borrow().clone(),
            n.cleanup.borrow_mut().take(),
        );
        out
    };

    if let Some(c) = prev_cleanup {
        c();
    }

    let outcome: Result<Option<Disposable>, ReactiveError> = match (&effect_fn, &value) {
        (Some(f), Some(v)) => {
            let mut f = f.borrow_mut();
            f(&**v, prev_value.as_deref())
        }
        _ => Ok(None),
    };

    // Upstream's `finally`: roll `prev_value` forward and clear `modified`
    // whether the apply phase succeeded or failed.
    {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(id) else { return };
        *n.prev_value.borrow_mut() = value;
        n.modified.set(false);
    }

    match outcome {
        Ok(next_cleanup) => {
            // The final cleanup is invoked by `dispose_children` at true
            // disposal; between reruns the previous one runs above.
            let nodes = rt.nodes();
            if let Some(n) = nodes.get(id) {
                *n.cleanup.borrow_mut() = next_cleanup;
            }
        }
        Err(user_err) => {
            // An uncaught apply-phase error is an unhandled application error:
            // caught by the nearest error boundary, and halting the reactive
            // system if there is none. It is *not* routed to the bundle's
            // error handler, which covers compute-phase errors only.
            let queue = {
                let nodes = rt.nodes();
                let Some(n) = nodes.get(id) else { return };
                let wrapped = ReactiveError::status(
                    id,
                    user_err
                        .unwrap_status()
                        .unwrap_or_else(|| Rc::new("effect error") as Rc<dyn std::any::Any>),
                );
                *n.error.borrow_mut() = Some(wrapped);
                n.status_flags.set(n.status_flags.get() | status::ERROR);
                n.queue.get()
            };
            let err = {
                let nodes = rt.nodes();
                nodes.get(id).and_then(|n| n.error.borrow().clone())
            };
            if !queue_notify(rt, queue, id, status::ERROR, status::ERROR, err.clone()) {
                halt_reactivity(rt, err.as_ref());
                panic_with(err);
            }
        }
    }
}
