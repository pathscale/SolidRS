//! Port of `tests/flush.test.ts`.

mod common;

use common::{Log, Spy};
use solid_rs::*;

// ---------------------------------------------------------------------------
// `describe("scheduler hygiene around throwing effects")`
// ---------------------------------------------------------------------------

/// `balances syncDepth when an effect throws inside flush(fn)`
///
/// Upstream's effect throws; here the apply phase returns `Err`, which is the
/// same escalation channel. The invariant under test is the same: an effect
/// that fails inside `flush(fn)` must not leak the sync-depth counter, because
/// a leaked counter stops `schedule()` from ever posting a microtask again.
#[test]
fn balances_sync_depth_when_an_effect_fails_inside_flush_with() {
    let (a, set_a) = create_signal(0);
    let (c, set_c) = create_signal(0);
    let seen_c = Log::<i32>::new();
    let sink = seen_c.clone();

    create_root(move |_| {
        // Fails without writing a signal, so the drain leaves no pending work —
        // this isolates the sync-depth leak from any stuck-`scheduled` symptom.
        try_create_effect(
            move |_| Ok(a.get()),
            move |v: &i32, _| {
                if *v == 1 {
                    Err(ReactiveError::msg("boom"))
                } else {
                    Ok(())
                }
            },
            None,
            EffectOptions::new(),
        );
        create_effect(move |_| c.get(), move |v: &i32, _| sink.push(*v));
    });
    flush();
    host::drive_microtasks();
    seen_c.reset();

    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        flush_with(|| set_a.set(1));
    }));
    assert!(failed.is_err(), "the uncaught error escapes flush");

    // The uncaught error halts the system by design; revive it so a leaked
    // sync-depth is the only thing that could stop `schedule()` from posting a
    // microtask again.
    reset_error_halt();
    set_c.set(3);
    host::drive_microtasks();
    assert_eq!(seen_c.get(), vec![3]);
}

/// `should batch updates`
#[test]
fn should_batch_updates() {
    let (x, set_x) = create_signal(10);
    let effect = Spy::<()>::new();
    let e = effect.clone();

    create_root(move |_| create_effect(move |_| x.get(), move |_v: &i32, _| e.hit()));
    flush();

    set_x.set(20);
    set_x.set(30);
    set_x.set(40);

    assert_eq!(effect.times(), 1);
    flush();
    assert_eq!(effect.times(), 2);
}

/// `should wait for queue to flush`
#[test]
fn should_wait_for_queue_to_flush() {
    let (x, set_x) = create_signal(10);
    let effect = Spy::<()>::new();
    let e = effect.clone();

    create_root(move |_| create_effect(move |_| x.get(), move |_v: &i32, _| e.hit()));
    flush();

    assert_eq!(effect.times(), 1);

    set_x.set(20);
    flush();
    assert_eq!(effect.times(), 2);

    set_x.set(30);
    flush();
    assert_eq!(effect.times(), 3);
}

/// `should not fail if called while flushing`
#[test]
fn should_not_fail_if_called_while_flushing() {
    let (a, set_a) = create_signal(10);
    let effect = Spy::<()>::new();
    let e = effect.clone();

    create_root(move |_| {
        create_effect(
            move |_| a.get(),
            move |_v: &i32, _| {
                e.hit();
                flush();
            },
        )
    });
    flush();

    assert_eq!(effect.times(), 1);

    set_a.set(20);
    flush();
    assert_eq!(effect.times(), 2);
}

/// `should run callback and flush before returning`
#[test]
fn should_run_callback_and_flush_before_returning() {
    let (x, set_x) = create_signal(10);
    let effect = Spy::<()>::new();
    let e = effect.clone();

    create_root(move |_| create_effect(move |_| x.get(), move |_v: &i32, _| e.hit()));
    flush();

    let probe = effect.clone();
    let result = flush_with(move || {
        set_x.set(20);
        assert_eq!(probe.times(), 1);
        "done"
    });

    assert_eq!(result, "done");
    assert_eq!(effect.times(), 2);
}

/// `nested flush(fn) drains at each level`
#[test]
fn nested_flush_with_drains_at_each_level() {
    let (x, set_x) = create_signal(10);
    let (y, set_y) = create_signal(10);
    let effect = Spy::<()>::new();
    let e = effect.clone();

    create_root(move |_| {
        create_effect(
            move |_| (x.get(), y.get()),
            move |_v: &(i32, i32), _| e.hit(),
        )
    });
    flush();

    let outer = effect.clone();
    flush_with(move || {
        set_x.set(20);
        assert_eq!(outer.times(), 1);

        let inner_probe = outer.clone();
        let inner = flush_with(move || {
            set_y.set(30);
            assert_eq!(inner_probe.times(), 1);
            1
        });

        assert_eq!(inner, 1);
        // The inner flush drained, so the effect already saw (20, 30).
        assert_eq!(outer.times(), 2);
    });

    // The outer drain finds nothing pending — no extra call.
    assert_eq!(effect.times(), 2);
}

/// Not upstream: pins the microtask seam this port has to provide, since there
/// is no event loop to post `queueMicrotask(flush)` onto.
#[test]
fn writes_settle_on_the_microtask_checkpoint_without_an_explicit_flush() {
    let (x, set_x) = create_signal(0);
    let seen = Log::<i32>::new();
    let sink = seen.clone();

    create_root(move |_| {
        create_effect(move |_| x.get(), move |v: &i32, _| sink.push(*v));
    });
    host::drive_microtasks();
    assert_eq!(seen.get(), vec![0]);

    // Two writes in one host turn collapse into a single update pass.
    set_x.set(1);
    set_x.set(2);
    assert!(host::has_pending_microtasks());
    host::drive_microtasks();
    assert_eq!(seen.get(), vec![0, 2]);
    assert!(!host::has_pending_microtasks());
}
