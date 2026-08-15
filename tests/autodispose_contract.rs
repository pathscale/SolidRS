//! Port of `tests/autodispose-contract.test.ts`.
//!
//! The contract: a pending computation's in-flight work counts as its own
//! observer, so losing the last subscriber mid-flight must not tear it down —
//! each teardown would re-execute the source on the next read (one fetch per
//! suspended re-read). `unlink_subs` skips autodispose while `STATUS_PENDING`,
//! and settling is that observer's release, which runs the same last-one-out
//! check every other release site runs.

mod common;

use common::{Counter, Log};
use solid_rs::*;

/// `an async memo read only via untrack from a suspending render-effect settles
/// once and returns the cached value across re-runs`
#[test]
fn async_memo_read_only_via_untrack_settles_once_and_returns_the_cached_value() {
    let compute_runs = Counter::new();
    let observed = Counter::new();
    let (tick, set_tick) = create_signal(0);
    let flights: Log<Resolver<i32>> = Log::new();
    let (r, sink) = (compute_runs.clone(), flights.clone());
    let obs = observed.clone();

    create_root(move |_| {
        let memo = create_async_memo(move |_| {
            r.inc();
            let (flight, resolver) = Flight::<i32>::new();
            sink.push(resolver);
            Async::Flight(flight)
        });

        // Mirrors the upstream shape: the effect tracks an unrelated signal to
        // give itself a reason to re-run, and reads the memo via `untrack` —
        // which raises until the memo resolves, after which it serves the
        // cached value.
        create_render_effect(
            move |_| {
                tick.get();
                if let Ok(v) = untrack(move || memo.try_get()) {
                    obs.reset();
                    for _ in 0..v {
                        obs.inc();
                    }
                }
            },
            move |_v: &(), _| {},
        );
    });

    flush();
    assert_eq!(compute_runs.get(), 1);

    flights.get()[0].resolve(123);
    host::drive_microtasks();
    assert_eq!(observed.get(), 123);
    assert_eq!(compute_runs.get(), 1);

    set_tick.set(1);
    flush();
    assert_eq!(observed.get(), 123);
    assert_eq!(compute_runs.get(), 1);

    set_tick.set(2);
    flush();
    set_tick.set(3);
    flush();

    assert_eq!(observed.get(), 123);
    assert_eq!(compute_runs.get(), 1, "settled once, cached thereafter");
}

/// `the suspension link added during the throw should not cause autodispose when
/// later removed by a successful re-run`
#[test]
fn the_suspension_link_added_during_the_throw_should_not_cause_autodispose() {
    let compute_runs = Counter::new();
    let (tick, set_tick) = create_signal(0);
    let flights: Log<Resolver<i32>> = Log::new();
    let (r, sink) = (compute_runs.clone(), flights.clone());

    create_root(move |_| {
        let memo = create_async_memo(move |_| {
            r.inc();
            let (flight, resolver) = Flight::<i32>::new();
            sink.push(resolver);
            Async::Flight(flight)
        });
        create_render_effect(
            move |_| {
                tick.get();
                let _ = untrack(move || memo.try_get());
            },
            move |_v: &(), _| {},
        );
    });

    flush();
    flights.get()[0].resolve(123);
    host::drive_microtasks();
    let runs_after_first_settle = compute_runs.get();

    set_tick.set(1);
    flush();
    set_tick.set(2);
    flush();

    assert_eq!(compute_runs.get(), runs_after_first_settle);
}

/// `subscriber churn while the promise is in flight neither tears down nor
/// re-executes`
#[test]
fn subscriber_churn_while_the_promise_is_in_flight_neither_tears_down_nor_re_executes() {
    let runs = Counter::new();
    let flights: Log<Resolver<i32>> = Log::new();
    let (r, sink) = (runs.clone(), flights.clone());

    // A LAZY memo (autodispose when unobserved) whose computation is async. The
    // in-flight work counts as an observer: losing the last subscriber
    // mid-flight must not tear the node down.
    let lazy = create_async_memo_with(
        move |_| {
            r.inc();
            let (flight, resolver) = Flight::<i32>::new();
            sink.push(resolver);
            Async::Flight(flight)
        },
        Options::default().lazy(true),
    );

    // First subscriber starts the computation, then goes away mid-flight.
    let dispose1 = create_root(move |dispose| {
        create_render_effect(move |_| lazy.try_get().ok(), move |_v: &Option<i32>, _| {});
        dispose
    });
    flush();
    assert_eq!(runs.get(), 1);
    dispose1.dispose();
    flush();

    // A later subscriber attaches to the SAME in-flight computation.
    let observed = Counter::new();
    let obs = observed.clone();
    let dispose2 = create_root(move |dispose| {
        create_render_effect(
            move |_| lazy.try_get().ok(),
            move |v: &Option<i32>, _| {
                if let Some(v) = v {
                    obs.reset();
                    for _ in 0..*v {
                        obs.inc();
                    }
                }
            },
        );
        dispose
    });
    flush();
    assert_eq!(runs.get(), 1, "attached to the in-flight work, did not restart");

    flights.get()[0].resolve(42);
    host::drive_microtasks();
    assert_eq!(observed.get(), 42);
    assert_eq!(runs.get(), 1);
    dispose2.dispose();
    flush();
}

/// `settling while unobserved releases the node (lazy teardown resumes once idle)`
#[test]
fn settling_while_unobserved_releases_the_node() {
    let runs = Counter::new();
    let flights: Log<Resolver<i32>> = Log::new();
    let (r, sink) = (runs.clone(), flights.clone());

    let lazy = create_async_memo_with(
        move |_| {
            r.inc();
            let (flight, resolver) = Flight::<i32>::new();
            sink.push(resolver);
            Async::Flight(flight)
        },
        Options::default().lazy(true),
    );

    let dispose1 = create_root(move |dispose| {
        create_render_effect(move |_| lazy.try_get().ok(), move |_v: &Option<i32>, _| {});
        dispose
    });
    flush();
    assert_eq!(runs.get(), 1);
    // Unobserved but in flight: stays alive (previous test) …
    dispose1.dispose();
    flush();

    // … but once it settles with no subscribers, the node releases — it must
    // not stay linked to its sources forever. A fresh subscriber recomputes
    // from scratch, which is lazy's normal idle behaviour.
    flights.get()[0].resolve(1);
    host::drive_microtasks();

    create_root(move |_| {
        create_render_effect(move |_| lazy.try_get().ok(), move |_v: &Option<i32>, _| {});
    });
    flush();
    assert_eq!(runs.get(), 2);
}

// ---------------------------------------------------------------------------
// Synchronous counterparts, kept from phase 1.
// ---------------------------------------------------------------------------

/// Not upstream. Pins the `AUTO_DISPOSE` gate on the synchronous side: a lazy
/// memo is torn down when its last subscriber goes away, and a fresh subscriber
/// recomputes it from scratch.
#[test]
fn lazy_memo_releases_on_last_subscriber_and_recomputes_for_the_next() {
    let runs = Counter::new();
    let r = runs.clone();
    let (x, _set_x) = create_signal(1);

    let lazy = create_memo_with(
        move |_| {
            r.inc();
            x.get()
        },
        Options::default().lazy(true),
    );

    assert_eq!(runs.get(), 0, "lazy: nothing computes until read");

    let dispose1 = create_root(move |dispose| {
        create_render_effect(move |_| lazy.get(), move |_v: &i32, _| {});
        dispose
    });
    flush();
    assert_eq!(runs.get(), 1);

    dispose1.dispose();
    flush();

    create_root(move |_| {
        create_render_effect(move |_| lazy.get(), move |_v: &i32, _| {});
    });
    flush();
    assert_eq!(runs.get(), 2);
}

/// Not upstream. An owned, non-lazy memo is *not* auto-disposed: it lives for
/// its owner's lifetime, which is the other half of the `AUTO_DISPOSE` gate.
#[test]
fn owned_eager_memo_is_not_released_when_it_loses_subscribers() {
    let runs = Counter::new();
    let r = runs.clone();
    let (x, set_x) = create_signal(1);

    let memo = create_root(move |_| {
        create_memo(move |_| {
            r.inc();
            x.get()
        })
    });
    assert_eq!(runs.get(), 1);

    assert_eq!(memo.get(), 1);
    assert_eq!(runs.get(), 1, "no subscribers, but owned: not released");

    set_x.set(2);
    flush();
    assert_eq!(memo.get(), 2);
    assert_eq!(runs.get(), 2);
}
