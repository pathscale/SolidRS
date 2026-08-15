//! Phase 2 foundation: async computeds suspend and settle.
//!
//! These are the async tests from `tests/createMemo.test.ts` and
//! `tests/createSignal.test.ts` that the foundation unblocks, translated per
//! the table in `async_source.rs` (`await Promise.resolve()` →
//! `host::drive_microtasks()`).

mod common;

use common::{Counter, Log};
use solid_rs::*;

/// `should throw when reading uninitialized async computed outside reactive scope`
#[test]
fn should_throw_when_reading_uninitialized_async_computed_outside_reactive_scope() {
    let (flight, resolver) = Flight::<i32>::new();
    let a = create_root(move |_| {
        let f = flight.clone();
        let a = create_async_memo(move |_| Async::Flight(f.clone()));
        create_render_effect(move |_| a.try_get().ok(), move |_v: &Option<i32>, _| {});
        a
    });

    assert!(a.try_get().is_err(), "uninitialized async read raises");

    resolver.resolve(42);
    host::drive_microtasks();
    assert_eq!(a.get(), 42);
}

/// `should return stale value when reading re-pending async computed outside reactive scope`
#[test]
fn should_return_stale_value_when_reading_re_pending_async_computed() {
    let (s, set) = create_signal(1);
    let flights: Log<Resolver<i32>> = Log::new();
    let sink = flights.clone();

    let a = create_root(move |_| {
        let a = create_async_memo(move |_| {
            let v = s.get();
            let (flight, resolver) = Flight::<i32>::new();
            sink.push(resolver.clone());
            let _ = v;
            Async::Flight(flight)
        });
        create_render_effect(move |_| a.try_get().ok(), move |_v: &Option<i32>, _| {});
        a
    });

    flights.get()[0].resolve(1);
    host::drive_microtasks();
    assert_eq!(a.get(), 1);

    set.set(2);
    flush();
    // The refetch is in flight; the committed channel still shows the old value.
    assert_eq!(a.get(), 1);

    flights.get()[1].resolve(2);
    host::drive_microtasks();
    assert_eq!(a.get(), 2);
}

/// A sync-resolved thenable never suspends. Port of the `syncThenable` contract.
#[test]
fn a_synchronously_resolved_flight_lands_without_suspending() {
    let runs = Counter::new();
    let r = runs.clone();
    let a = create_root(move |_| {
        create_async_memo(move |_| {
            r.inc();
            Async::Flight(Flight::ready(7))
        })
    });
    assert_eq!(a.get(), 7, "no suspension, no boundary, no transition");
    assert_eq!(runs.get(), 1);
}

/// `Async::Ready` is the non-thenable return: a plain sync value.
#[test]
fn a_ready_value_behaves_like_a_sync_memo() {
    let (x, set_x) = create_signal(1);
    let a = create_root(move |_| create_async_memo(move |_| Async::Ready(x.get() * 10)));
    assert_eq!(a.get(), 10);
    set_x.set(2);
    flush();
    assert_eq!(a.get(), 20);
}

/// `should handle streams` — each push is one `yield`.
#[test]
fn should_handle_streams() {
    let (stream, producer) = Streaming::<i32>::new();
    let seen = Log::<i32>::new();
    let sink = seen.clone();

    let a = create_root(move |_| {
        let s = stream.clone();
        let a = create_async_memo(move |_| Async::Stream(s.clone()));
        create_render_effect(
            move |_| a.try_get().ok(),
            move |v: &Option<i32>, _| {
                if let Some(v) = v {
                    sink.push(*v);
                }
            },
        );
        a
    });

    flush();
    assert!(a.try_get().is_err(), "no yield yet: still loading");

    producer.push(1);
    host::drive_microtasks();
    assert_eq!(a.get(), 1);

    producer.push(2);
    host::drive_microtasks();
    assert_eq!(a.get(), 2);

    producer.push(3);
    host::drive_microtasks();
    assert_eq!(a.get(), 3);
    assert_eq!(seen.get(), vec![1, 2, 3]);
}

/// A rejected flight puts the memo into the error state.
#[test]
fn a_rejected_flight_errors_the_memo() {
    let (flight, resolver) = Flight::<i32>::new();
    // A *user* effect: a compute-phase error with no handler is logged and the
    // run skipped, and the system keeps going (A2). A render effect would
    // escalate to the nearest error boundary, and halt without one (A5).
    let a = create_root(move |_| {
        let f = flight.clone();
        let a = create_async_memo(move |_| Async::Flight(f.clone()));
        create_effect(move |_| a.try_get().ok(), move |_v: &Option<i32>, _| {});
        a
    });

    resolver.reject(ReactiveError::msg("boom"));
    host::drive_microtasks();

    let err = a.try_get().expect_err("errored");
    assert_eq!(err.message(), "boom");
}

/// `isPending` is `false` for an uninitialized source: it is *loading*, not
/// pending — there is no observable value yet to be non-final (A19 exception 1).
#[test]
fn is_pending_is_false_during_the_initial_load() {
    let (flight, resolver) = Flight::<i32>::new();
    let a = create_root(move |_| {
        let f = flight.clone();
        let a = create_async_memo(move |_| Async::Flight(f.clone()));
        create_render_effect(move |_| a.try_get().ok(), move |_v: &Option<i32>, _| {});
        a
    });
    flush();

    assert!(
        !is_pending(move || {
            let _ = a.try_get();
        }),
        "initial load is loading, not pending"
    );

    resolver.resolve(1);
    host::drive_microtasks();
    assert!(!is_pending(move || {
        let _ = a.try_get();
    }));
}

/// A superseded flight's late arrival is dropped — upstream's
/// `el._inFlight !== result` guard.
#[test]
fn a_superseded_flight_does_not_clobber_a_newer_one() {
    let (s, set) = create_signal(1);
    let flights: Log<Resolver<i32>> = Log::new();
    let sink = flights.clone();

    let a = create_root(move |_| {
        let a = create_async_memo(move |_| {
            let _ = s.get();
            let (flight, resolver) = Flight::<i32>::new();
            sink.push(resolver);
            Async::Flight(flight)
        });
        create_render_effect(move |_| a.try_get().ok(), move |_v: &Option<i32>, _| {});
        a
    });

    // Start a second fetch before the first settles.
    set.set(2);
    flush();
    assert_eq!(flights.len(), 2);

    // The stale first flight lands last and must be ignored.
    flights.get()[1].resolve(200);
    host::drive_microtasks();
    flights.get()[0].resolve(100);
    host::drive_microtasks();

    assert_eq!(a.get(), 200, "the superseded flight was dropped");
}
