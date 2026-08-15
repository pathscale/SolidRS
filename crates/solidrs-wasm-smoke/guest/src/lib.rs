//! The guest: a real WebAssembly module that *uses* the reactive graph.
//!
//! Compiled as a `cdylib` for `wasm32-unknown-unknown` and instantiated by the
//! host under wasmi. Two exports, deliberately covering the two halves of the
//! port:
//!
//! * [`run`] — the synchronous core: signal → memo → effect, flushed, with the
//!   value the effect observed handed back.
//! * [`run_async`] — the phase-2 foundation: an async memo that suspends on a
//!   [`Flight`], settles when the resolver fires, and reveals through the
//!   microtask queue that a host drives.
//!
//! Both return an `i32` the host asserts on, so a wrong answer fails as loudly
//! as a trap would.

use std::cell::Cell;
use std::rc::Rc;

use solid_rs::*;

/// Synchronous smoke: `signal → memo → effect`, flush, return what the effect
/// saw.
///
/// Expected: `1 → doubled = 2`, then `set 20 → doubled = 40`. Returns `40`.
#[no_mangle]
pub extern "C" fn run() -> i32 {
    let observed = Rc::new(Cell::new(0i32));
    let sink = observed.clone();

    let (count, set_count) = create_signal(1);
    let doubled = create_memo(move |_| count.get() * 2);

    create_root(move |_| {
        create_effect(
            move |_| doubled.get(),
            move |v: &i32, _| sink.set(*v),
        );
    });
    flush();
    if observed.get() != 2 {
        return -1;
    }

    set_count.set(20);
    flush();
    observed.get()
}

/// Async smoke: an async memo suspends, then settles through the microtask
/// queue.
///
/// Exercises the piece with no JS platform underneath it — the `Flight` /
/// microtask substitute for a `Promise`. Returns `7` on success, a negative
/// code identifying the failed step otherwise.
#[no_mangle]
pub extern "C" fn run_async() -> i32 {
    let (flight, resolver) = Flight::<i32>::new();

    let data = create_root(move |_| {
        let f = flight.clone();
        create_async_memo(move |_| Async::Flight(f.clone()))
    });

    // Suspended: an uninitialized async read raises rather than inventing a
    // value.
    if data.try_get().is_ok() {
        return -1;
    }
    // …and it is *loading*, not pending — no observable value exists yet.
    if is_pending(move || {
        let _ = data.try_get();
    }) {
        return -2;
    }

    resolver.resolve(7);
    host::drive_microtasks();

    match data.try_get() {
        Ok(v) => v,
        Err(_) => -3,
    }
}

/// Ownership smoke: disposal actually runs cleanups inside a wasm module.
///
/// Returns the number of cleanups observed (expected `2`).
#[no_mangle]
pub extern "C" fn run_dispose() -> i32 {
    let cleanups = Rc::new(Cell::new(0i32));

    let d = {
        let a = cleanups.clone();
        let b = cleanups.clone();
        create_root(move |dispose| {
            on_cleanup(move || a.set(a.get() + 1));
            create_effect(
                move |_| {
                    let b = b.clone();
                    on_cleanup(move || b.set(b.get() + 1));
                },
                move |_v: &(), _| {},
            );
            dispose
        })
    };
    flush();
    d.dispose();
    cleanups.get()
}
