//! Port of `tests/createEffect.test.ts`.

mod common;

use common::{Counter, Log, Order, Spy};
use solid_rs::*;

/// `should run effect`
#[test]
fn should_run_effect() {
    let (x, set_x) = create_signal(0);
    let compute = Spy::<()>::new();
    let effect = Spy::<(i32, Option<i32>)>::new();
    let (c, e) = (compute.clone(), effect.clone());

    create_root(move |_| {
        create_effect(
            move |_| {
                c.hit();
                x.get()
            },
            move |v: &i32, p: Option<&i32>| e.call((*v, p.copied())),
        )
    });
    assert_eq!(compute.times(), 1);
    assert_eq!(effect.times(), 0);
    flush();
    assert_eq!(compute.times(), 1);
    assert_eq!(effect.times(), 1);
    assert!(effect.was_called_with(&(0, None)));

    set_x.set(1);
    flush();
    assert_eq!(compute.times(), 2);
    assert_eq!(effect.times(), 2);
    assert!(effect.was_called_with(&(1, Some(0))));
}

/// `should run effect on change`
#[test]
fn should_run_effect_on_change() {
    let effect = Spy::<()>::new();
    let e = effect.clone();

    let (x, set_x) = create_signal(10);
    let (y, set_y) = create_signal(10);

    let a = create_memo(move |_| x.get() + y.get());
    let b = create_memo(move |_| a.get());

    create_root(move |_| create_effect(move |_| b.get(), move |_v: &i32, _| e.hit()));

    assert_eq!(effect.times(), 0);

    set_x.set(20);
    flush();
    assert_eq!(effect.times(), 1);

    set_y.set(20);
    flush();
    assert_eq!(effect.times(), 2);

    set_x.set(20);
    set_y.set(20);
    flush();
    assert_eq!(effect.times(), 2);
}

/// `should handle nested effect`
#[test]
fn should_handle_nested_effect() {
    let (x, set_x) = create_signal(0);
    let (y, set_y) = create_signal(0);

    let outer_effect = Spy::<()>::new();
    let inner_effect = Spy::<()>::new();
    let inner_pure_dispose = Spy::<()>::new();
    let inner_effect_dispose = Spy::<()>::new();
    let (oe, ie, ipd, ied) = (
        outer_effect.clone(),
        inner_effect.clone(),
        inner_pure_dispose.clone(),
        inner_effect_dispose.clone(),
    );

    let stop = create_root(move |dispose| {
        create_effect(
            move |_| {
                x.get();
                let ie = ie.clone();
                let ipd = ipd.clone();
                let ied = ied.clone();
                create_effect(
                    move |_| {
                        y.get();
                        let ipd = ipd.clone();
                        on_cleanup(move || ipd.hit());
                    },
                    move |_v: &(), _| {
                        ie.hit();
                        let ied = ied.clone();
                        on_dispose(move || ied.hit())
                    },
                );
            },
            move |_v: &(), _| oe.hit(),
        );
        dispose
    });

    flush();
    assert_eq!(outer_effect.times(), 1);
    assert_eq!(inner_effect.times(), 1);
    assert_eq!(inner_pure_dispose.times(), 0);
    assert_eq!(inner_effect_dispose.times(), 0);

    set_y.set(1);
    flush();
    assert_eq!(outer_effect.times(), 1);
    assert_eq!(inner_effect.times(), 2);
    assert_eq!(inner_pure_dispose.times(), 1);
    assert_eq!(inner_effect_dispose.times(), 1);

    set_y.set(2);
    flush();
    assert_eq!(outer_effect.times(), 1);
    assert_eq!(inner_effect.times(), 3);
    assert_eq!(inner_pure_dispose.times(), 2);
    assert_eq!(inner_effect_dispose.times(), 2);

    inner_effect.reset();
    inner_pure_dispose.reset();
    inner_effect_dispose.reset();

    set_x.set(1);
    flush();
    assert_eq!(outer_effect.times(), 2);
    assert_eq!(inner_effect.times(), 1, "a new inner effect is created");
    assert_eq!(inner_pure_dispose.times(), 1);
    assert_eq!(inner_effect_dispose.times(), 1);

    set_y.set(3);
    flush();
    assert_eq!(outer_effect.times(), 2);
    assert_eq!(inner_effect.times(), 2);
    assert_eq!(inner_pure_dispose.times(), 2);
    assert_eq!(inner_effect_dispose.times(), 2);

    stop.dispose();
    set_x.set(10);
    set_y.set(10);
    assert_eq!(outer_effect.times(), 2);
    assert_eq!(inner_effect.times(), 2);
    assert_eq!(inner_pure_dispose.times(), 3);
    assert_eq!(inner_effect_dispose.times(), 3);
}

/// `should stop effect`
#[test]
fn should_stop_effect() {
    let effect = Spy::<()>::new();
    let e = effect.clone();
    let (x, set_x) = create_signal(10);

    let stop = create_root(move |dispose| {
        create_effect(move |_| x.get(), move |_v: &i32, _| e.hit());
        dispose
    });

    stop.dispose();

    set_x.set(20);
    flush();
    assert_eq!(effect.times(), 0);
}

/// `should throw on invalid cleanup values`
#[test]
#[ignore = "not representable: the apply phase returns `impl IntoCleanup`, so an invalid cleanup \
            value is a compile error rather than a runtime throw"]
fn should_throw_on_invalid_cleanup_values() {}

/// `should run all disposals before each new run`
#[test]
fn should_run_all_disposals_before_each_new_run() {
    let effect = Spy::<()>::new();
    let dispose_a = Spy::<()>::new();
    let dispose_b = Spy::<()>::new();
    let dispose_c = Spy::<()>::new();
    let (e, da, db, dc) = (
        effect.clone(),
        dispose_a.clone(),
        dispose_b.clone(),
        dispose_c.clone(),
    );

    let (x, set_x) = create_signal(0);

    create_root(move |_| {
        create_effect(
            move |_| {
                let da = da.clone();
                on_cleanup(move || da.hit());
                let db = db.clone();
                on_cleanup(move || db.hit());
                x.get()
            },
            move |_v: &i32, _| {
                e.hit();
                let dc = dc.clone();
                on_dispose(move || dc.hit())
            },
        )
    });
    flush();

    assert_eq!(effect.times(), 1);
    assert_eq!(dispose_a.times(), 0);
    assert_eq!(dispose_b.times(), 0);
    assert_eq!(dispose_c.times(), 0);

    for i in 1..=3 {
        set_x.set(i);
        flush();
        assert_eq!(effect.times(), (i + 1) as usize);
        assert_eq!(dispose_a.times(), i as usize);
        assert_eq!(dispose_b.times(), i as usize);
        assert_eq!(dispose_c.times(), i as usize);
    }
}

/// `should dispose of nested effect`
#[test]
fn should_dispose_of_nested_effect() {
    let (x, set_x) = create_signal(0);
    let inner_effect = Spy::<i32>::new();
    let ie = inner_effect.clone();

    let stop = create_root(move |dispose| {
        create_effect(
            move |_| {
                let ie = ie.clone();
                create_effect(move |_| x.get(), move |v: &i32, _| ie.call(*v));
            },
            move |_v: &(), _| {},
        );
        dispose
    });

    stop.dispose();

    set_x.set(10);
    flush();
    assert_eq!(inner_effect.times(), 0);
    assert!(!inner_effect.was_called_with(&10));
}

/// `should conditionally observe`
#[test]
fn should_conditionally_observe() {
    let (x, set_x) = create_signal(0);
    let (y, set_y) = create_signal(0);
    let (condition, set_condition) = create_signal(true);

    let a = create_memo(move |_| if condition.get() { x.get() } else { y.get() });
    let effect = Spy::<()>::new();
    let e = effect.clone();

    create_root(move |_| create_effect(move |_| a.get(), move |_v: &i32, _| e.hit()));
    flush();

    assert_eq!(effect.times(), 1);

    set_y.set(1);
    flush();
    assert_eq!(effect.times(), 1);

    set_x.set(1);
    flush();
    assert_eq!(effect.times(), 2);

    set_condition.set(false);
    flush();
    assert_eq!(effect.times(), 2);

    set_y.set(2);
    flush();
    assert_eq!(effect.times(), 3);

    set_x.set(3);
    flush();
    assert_eq!(effect.times(), 3);
}

/// `should dispose of nested conditional effect`
#[test]
fn should_dispose_of_nested_conditional_effect() {
    let (condition, set_condition) = create_signal(true);
    let dispose_a = Spy::<()>::new();
    let dispose_b = Spy::<()>::new();
    let (da, db) = (dispose_a.clone(), dispose_b.clone());

    create_root(move |_| {
        create_effect(
            move |_| {
                if condition.get() {
                    let da = da.clone();
                    create_effect(
                        move |_| {
                            let da = da.clone();
                            on_cleanup(move || da.hit());
                        },
                        move |_v: &(), _| {},
                    );
                } else {
                    let db = db.clone();
                    create_effect(
                        move |_| {
                            let db = db.clone();
                            on_cleanup(move || db.hit());
                        },
                        move |_v: &(), _| {},
                    );
                }
            },
            move |_v: &(), _| {},
        )
    });
    flush();
    set_condition.set(false);
    flush();
    assert_eq!(dispose_a.times(), 1);
}

/// `should handle looped effects`
///
/// Upstream reference: <https://github.com/preactjs/signals/issues/152>
#[test]
fn should_handle_looped_effects() {
    let values = Log::<i32>::new();
    let loop_count = Counter::new();
    loop_count.inc();
    loop_count.inc(); // loop = 2

    let (value, set_value) = create_signal(0);
    let (v, lc) = (values.clone(), loop_count.clone());

    create_root(move |_| {
        create_effect(
            move |_| {
                v.push(value.get());
                for i in 0..lc.get() {
                    let v = v.clone();
                    create_effect(
                        move |_| value.get() + i as i32,
                        move |val: &i32, _| v.push(*val),
                    );
                }
            },
            move |_v: &(), _| {},
        )
    });

    flush();
    assert_eq!(values.get(), vec![0, 0, 1]);

    loop_count.reset();
    loop_count.inc(); // loop = 1
    values.reset();
    set_value.set(1);
    flush();
    assert_eq!(values.get(), vec![1, 1]);

    values.reset();
    set_value.set(2);
    flush();
    assert_eq!(values.get(), vec![2, 2]);
}

/// `should apply changes in effect in same flush`
#[test]
fn should_apply_changes_in_effect_in_same_flush() {
    let (x, set_x) = create_signal(0);
    let (y, set_y) = create_signal(0);

    let a = create_memo(move |_| x.get() + 1);
    let b = create_memo(move |_| a.get() + 2);

    create_root(move |_| {
        create_effect(
            move |_| y.get(),
            move |_v: &i32, _| set_x.update(|n| n + 1),
        )
    });
    flush();

    assert_eq!(x.get(), 1);
    assert_eq!(b.get(), 4);
    assert_eq!(a.get(), 2);

    set_y.set(1);
    flush();

    assert_eq!(x.get(), 2);
    assert_eq!(b.get(), 5);
    assert_eq!(a.get(), 3);

    set_y.set(2);
    flush();

    assert_eq!(x.get(), 3);
    assert_eq!(b.get(), 6);
    assert_eq!(a.get(), 4);
}

/// `should run parent effect before child effect`
#[test]
fn should_run_parent_effect_before_child_effect() {
    let (x, set_x) = create_signal(0);
    let condition = create_memo(move |_| x.get());

    let calls = Counter::new();
    let c = calls.clone();

    create_root(move |_| {
        create_effect(
            move |_| {
                let c = c.clone();
                create_effect(
                    move |_| {
                        x.get();
                        c.inc();
                    },
                    move |_v: &(), _| {},
                );
                condition.get();
            },
            move |_v: &(), _| {},
        )
    });

    set_x.set(1);
    flush();
    assert_eq!(calls.get(), 2);
}

/// `should run render effect before user effects`
#[test]
fn should_run_render_effect_before_user_effects() {
    let (x, set_x) = create_signal(0);
    let mark = Order::new();
    let (m1, m2) = (mark.clone(), mark.clone());

    create_root(move |_| {
        create_effect(move |_| x.get(), move |_v: &i32, _| m1.push("b"));
        create_render_effect(move |_| x.get(), move |_v: &i32, _| m2.push("a"));
    });

    flush();
    assert_eq!(mark.get(), "ab");
    set_x.set(1);
    flush();
    assert_eq!(mark.get(), "abab");
}

/// `should see consistent values with latest() and direct read on async resolve`
#[test]
#[ignore = "phase 2: latest() is ported, but this pins latest-vs-direct consistency across a \
            transition-held commit, which needs lanes.ts"]
fn should_see_consistent_values_with_latest_and_direct_read_on_async_resolve() {}

/// `should defer user effects with the defer option`
#[test]
fn should_defer_user_effects_with_the_defer_option() {
    let mark = Order::new();
    let m = mark.clone();
    let (x, set_x) = create_signal(0);
    create_root(move |_| {
        create_effect_with(
            move |_| x.get(),
            move |_v: &i32, _| m.push("b"),
            EffectOptions::new().defer(true),
        );
    });
    flush();
    assert_eq!(mark.get(), "");
    set_x.set(1);
    flush();
    assert_eq!(mark.get(), "b");
}

// ---------------------------------------------------------------------------
// `describe("schedule option")`
// ---------------------------------------------------------------------------

/// `does not run initial render-effect callback synchronously when schedule: true`
#[test]
fn does_not_run_initial_render_effect_callback_synchronously_when_schedule_true() {
    let calls = Counter::new();
    let c = calls.clone();
    let inner = calls.clone();
    create_root(move |_| {
        create_render_effect_with(
            move |_| 1,
            move |_v: &i32, _| {
                c.inc();
            },
            EffectOptions::new().schedule(true),
        );
        assert_eq!(inner.get(), 0);
    });
    flush();
    assert_eq!(calls.get(), 1);
}

/// `runs initial render-effect callback synchronously without schedule (default)`
#[test]
fn runs_initial_render_effect_callback_synchronously_without_schedule() {
    let calls = Counter::new();
    let c = calls.clone();
    let inner = calls.clone();
    create_root(move |_| {
        create_render_effect(
            move |_| 1,
            move |_v: &i32, _| {
                c.inc();
            },
        );
        assert_eq!(inner.get(), 1);
    });
    flush();
    assert_eq!(calls.get(), 1);
}

/// `holds the scheduled callback until a pending async source resolves`
#[test]
#[ignore = "blocked on boundaries.ts: the scheduled callback is held by a boundary"]
fn holds_the_scheduled_callback_until_a_pending_async_source_resolves() {}

/// `subsequent changes after the scheduled initial run behave like a normal render effect`
#[test]
fn subsequent_changes_after_the_scheduled_initial_run_behave_like_a_normal_render_effect() {
    let (x, set_x) = create_signal(0);
    let received = Log::<i32>::new();
    let r = received.clone();

    create_root(move |_| {
        create_render_effect_with(
            move |_| x.get(),
            move |v: &i32, _| r.push(*v),
            EffectOptions::new().schedule(true),
        );
    });

    flush();
    assert_eq!(received.get(), vec![0]);

    set_x.set(1);
    flush();
    assert_eq!(received.get(), vec![0, 1]);

    set_x.set(2);
    flush();
    assert_eq!(received.get(), vec![0, 1, 2]);
}

/// `schedule: true + defer: true still defers (defer wins)`
#[test]
fn schedule_true_plus_defer_true_still_defers() {
    let (x, set_x) = create_signal(0);
    let received = Log::<i32>::new();
    let r = received.clone();

    create_root(move |_| {
        create_render_effect_with(
            move |_| x.get(),
            move |v: &i32, _| r.push(*v),
            EffectOptions::new().schedule(true).defer(true),
        );
    });

    flush();
    assert_eq!(received.get(), Vec::<i32>::new());

    set_x.set(1);
    flush();
    assert_eq!(received.get(), vec![1]);
}

// ---------------------------------------------------------------------------
// `describe("final returned cleanup runs at the effect's own disposal")`
//
// These pin implementation ordering so changes are deliberate; disposal order
// is NOT a documented guarantee to end users.
// ---------------------------------------------------------------------------

/// `runs once at disposal, after per-rerun cleanups`
#[test]
fn runs_once_at_disposal_after_per_rerun_cleanups() {
    let (x, set_x) = create_signal(0);
    let cleanup = Spy::<()>::new();
    let c = cleanup.clone();

    let dispose = create_root(move |dispose| {
        create_effect(
            move |_| x.get(),
            move |_v: &i32, _| {
                let c = c.clone();
                on_dispose(move || c.hit())
            },
        );
        dispose
    });
    flush();
    assert_eq!(cleanup.times(), 0);

    set_x.set(1);
    flush();
    assert_eq!(cleanup.times(), 1, "previous run's cleanup");

    dispose.dispose();
    assert_eq!(cleanup.times(), 2, "final cleanup, exactly once");

    set_x.set(2);
    flush();
    assert_eq!(cleanup.times(), 2);
}

/// `fires in unwind order at the effect's structural position`
#[test]
fn fires_in_unwind_order_at_the_effects_structural_position() {
    let order = Log::<&'static str>::new();
    let (x, _set_x) = create_signal(0);
    let (o1, o2, o3, o4) = (order.clone(), order.clone(), order.clone(), order.clone());

    let dispose = create_root(move |dispose| {
        on_cleanup(move || o1.push("onCleanup:before"));
        create_effect(
            move |_| x.get(),
            move |_v: &i32, _| {
                let o = o2.clone();
                on_dispose(move || o.push("effect:A"))
            },
        );
        create_effect(
            move |_| x.get(),
            move |_v: &i32, _| {
                let o = o3.clone();
                on_dispose(move || o.push("effect:B"))
            },
        );
        on_cleanup(move || o4.push("onCleanup:after"));
        dispose
    });
    flush();
    dispose.dispose();

    // Children unwind newest-first at their positions, then the owner's own
    // disposal list runs FIFO.
    assert_eq!(
        order.get(),
        vec!["effect:B", "effect:A", "onCleanup:before", "onCleanup:after"]
    );
}

/// `early individual disposal fires the cleanup once; outer disposal does not re-run it`
#[test]
fn early_individual_disposal_fires_the_cleanup_once() {
    let (x, _set_x) = create_signal(0);
    let cleanup = Spy::<()>::new();
    let c = cleanup.clone();

    let inner_slot: std::rc::Rc<std::cell::Cell<Option<Dispose>>> =
        std::rc::Rc::new(std::cell::Cell::new(None));
    let slot = inner_slot.clone();

    let dispose_outer = create_root(move |dispose| {
        create_root(move |d| {
            slot.set(Some(d));
            create_effect(
                move |_| x.get(),
                move |_v: &i32, _| {
                    let c = c.clone();
                    on_dispose(move || c.hit())
                },
            );
        });
        dispose
    });
    flush();

    inner_slot.get().expect("inner root").dispose();
    assert_eq!(cleanup.times(), 1);

    dispose_outer.dispose();
    assert_eq!(cleanup.times(), 1);
}

/// `render effect final cleanup follows the same rule`
#[test]
fn render_effect_final_cleanup_follows_the_same_rule() {
    let (x, set_x) = create_signal(0);
    let cleanup = Spy::<()>::new();
    let c = cleanup.clone();

    let dispose = create_root(move |dispose| {
        create_render_effect(
            move |_| x.get(),
            move |_v: &i32, _| {
                let c = c.clone();
                on_dispose(move || c.hit())
            },
        );
        dispose
    });
    flush();

    set_x.set(1);
    flush();
    assert_eq!(cleanup.times(), 1);

    dispose.dispose();
    assert_eq!(cleanup.times(), 2);
}
