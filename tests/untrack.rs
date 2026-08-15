//! Port of `tests/untrack.test.ts`.

mod common;

use common::Spy;
use solid_rs::*;

/// `should not create dependency`
#[test]
fn should_not_create_dependency() {
    let effect = Spy::<()>::new();
    let memo = Spy::<()>::new();
    let (e, m) = (effect.clone(), memo.clone());

    let (x, set_x) = create_signal(10);

    let a = create_memo(move |_| x.get() + 10);
    let b = create_memo(move |_| {
        m.hit();
        untrack(move || a.get()) + 10
    });

    create_root(move |_| {
        create_effect(
            move |_| {
                e.hit();
                assert_eq!(untrack(move || x.get()), 10);
                assert_eq!(untrack(move || a.get()), 20);
                assert_eq!(untrack(move || b.get()), 30);
            },
            move |_v: &(), _| {},
        )
    });
    flush();

    assert_eq!(effect.times(), 1);
    assert_eq!(memo.times(), 1);

    set_x.set(20);
    flush();
    assert_eq!(effect.times(), 1);
    assert_eq!(memo.times(), 1);
}

/// `should not affect deep dependency being created`
#[test]
fn should_not_affect_deep_dependency_being_created() {
    let effect = Spy::<()>::new();
    let memo = Spy::<()>::new();
    let (e, m) = (effect.clone(), memo.clone());

    let (x, set_x) = create_signal(10);
    let (y, set_y) = create_signal(10);
    let (z, set_z) = create_signal(10);

    let a = create_memo(move |_| {
        m.hit();
        x.get() + untrack(move || y.get()) + untrack(move || z.get()) + 10
    });

    create_root(move |_| {
        create_effect(
            move |_| {
                e.hit();
                assert_eq!(untrack(move || x.get()), 10);
                assert_eq!(untrack(move || a.get()), 40);
            },
            move |_v: &(), _| {},
        )
    });
    flush();

    assert_eq!(effect.times(), 1);
    assert_eq!(a.get(), 40);
    assert_eq!(memo.times(), 1);

    set_x.set(20);
    flush();
    assert_eq!(effect.times(), 1);
    assert_eq!(a.get(), 50);
    assert_eq!(memo.times(), 2);

    set_y.set(20);
    flush();
    assert_eq!(effect.times(), 1);
    assert_eq!(a.get(), 60);
    assert_eq!(memo.times(), 3);

    set_z.set(20);
    flush();
    assert_eq!(effect.times(), 1);
    assert_eq!(a.get(), 70);
    assert_eq!(memo.times(), 4);
}

/// `should track owner across peeks`
///
/// `untrack` suspends *tracking*, not *ownership*: primitives created inside
/// still belong to the surrounding owner and are disposed with it.
#[test]
fn should_track_owner_across_peeks() {
    let (x, set_x) = create_signal(0);

    let child_compute = Spy::<i32>::new();
    let child_dispose = Spy::<()>::new();
    let (cc, cd) = (child_compute.clone(), child_dispose.clone());

    let dispose = create_root(move |dispose| {
        untrack(move || {
            let a = create_memo(move |_| x.get() * 2);
            create_root(move |_| {
                create_effect(
                    move |_| {
                        cc.call(a.get());
                        let cd = cd.clone();
                        on_cleanup(move || cd.hit());
                    },
                    move |_v: &(), _| {},
                )
            });
        });
        dispose
    });
    flush();

    set_x.set(1);
    flush();
    assert!(child_compute.was_called_with(&2));
    assert_eq!(child_dispose.times(), 1);

    dispose.dispose();
    assert_eq!(child_dispose.times(), 2);

    set_x.set(2);
    flush();
    assert!(!child_compute.was_called_with(&4));
    assert_eq!(child_dispose.times(), 2);
}
