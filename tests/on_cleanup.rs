//! Port of `tests/onCleanup.test.ts`, plus `tests/getOwner.test.ts` and
//! `tests/runWithOwner.test.ts` — three short owner-lifecycle files kept
//! together.

mod common;

use common::{Counter, Log, Spy};
use solid_rs::*;

// ---------------------------------------------------------------------------
// onCleanup.test.ts
// ---------------------------------------------------------------------------

/// `should be invoked when computation is disposed`
#[test]
fn should_be_invoked_when_computation_is_disposed() {
    let dispose_a = Spy::<()>::new();
    let dispose_b = Spy::<()>::new();
    let dispose_c = Spy::<()>::new();
    let (a, b, c) = (dispose_a.clone(), dispose_b.clone(), dispose_c.clone());

    let stop = create_root(move |dispose| {
        create_effect(
            move |_| {
                let (a, b, c) = (a.clone(), b.clone(), c.clone());
                on_cleanup(move || a.hit());
                on_cleanup(move || b.hit());
                on_cleanup(move || c.hit());
            },
            move |_v: &(), _| {},
        );
        dispose
    });
    flush();

    stop.dispose();

    assert!(dispose_a.times() > 0);
    assert!(dispose_b.times() > 0);
    assert!(dispose_c.times() > 0);
}

/// `should not trigger wrong onCleanup`
#[test]
fn should_not_trigger_wrong_on_cleanup() {
    let dispose = Spy::<()>::new();
    let d = dispose.clone();

    create_root(move |_| {
        create_effect(
            move |_| {
                let d = d.clone();
                on_cleanup(move || d.hit());
            },
            move |_v: &(), _| {},
        );

        let stop = create_root(|dispose| {
            create_effect(move |_| {}, move |_v: &(), _| {});
            dispose
        });

        stop.dispose();
        flush();

        assert_eq!(dispose.times(), 0);
    });
}

/// `should clean up in reverse order`
#[test]
fn should_clean_up_in_reverse_order() {
    let calls = Counter::new();
    let parent_at = Counter::new();
    let a_at = Counter::new();
    let b_at = Counter::new();
    let (c1, c2, c3) = (calls.clone(), calls.clone(), calls.clone());
    let (pa, aa, ba) = (parent_at.clone(), a_at.clone(), b_at.clone());

    let stop = create_root(move |dispose| {
        create_effect(
            move |_| {
                let (c1, pa) = (c1.clone(), pa.clone());
                on_cleanup(move || {
                    let n = c1.inc();
                    for _ in 0..n {
                        pa.inc();
                    }
                });

                let (c2, aa) = (c2.clone(), aa.clone());
                create_effect(
                    move |_| {
                        let (c2, aa) = (c2.clone(), aa.clone());
                        on_cleanup(move || {
                            let n = c2.inc();
                            for _ in 0..n {
                                aa.inc();
                            }
                        });
                    },
                    move |_v: &(), _| {},
                );

                let (c3, ba) = (c3.clone(), ba.clone());
                create_effect(
                    move |_| {
                        let (c3, ba) = (c3.clone(), ba.clone());
                        on_cleanup(move || {
                            let n = c3.inc();
                            for _ in 0..n {
                                ba.inc();
                            }
                        });
                    },
                    move |_v: &(), _| {},
                );
            },
            move |_v: &(), _| {},
        );
        dispose
    });
    flush();

    stop.dispose();

    // B ran first (call #1), then A (#2), then the parent (#3).
    assert_eq!(b_at.get(), 1);
    assert_eq!(a_at.get(), 2);
    assert_eq!(parent_at.get(), 3);
}

/// `should dispose all roots`
#[test]
fn should_dispose_all_roots() {
    let disposals = Log::<&'static str>::new();
    let d = disposals.clone();

    let dispose = create_root(move |dispose| {
        let d1 = d.clone();
        create_root(move |_| {
            let d = d1.clone();
            on_cleanup(move || d.push("SUBTREE 1"));
            for tag in ["+A1", "+B1", "+C1"] {
                let d = d1.clone();
                create_effect(
                    move |_| {
                        let d = d.clone();
                        on_cleanup(move || d.push(tag));
                    },
                    move |_v: &(), _| {},
                );
            }
        });

        let d2 = d.clone();
        create_root(move |_| {
            let d = d2.clone();
            on_cleanup(move || d.push("SUBTREE 2"));
            for tag in ["+A2", "+B2", "+C2"] {
                let d = d2.clone();
                create_effect(
                    move |_| {
                        let d = d.clone();
                        on_cleanup(move || d.push(tag));
                    },
                    move |_v: &(), _| {},
                );
            }
        });

        let d3 = d.clone();
        on_cleanup(move || d3.push("ROOT"));

        dispose
    });

    flush();
    dispose.dispose();

    assert_eq!(
        disposals.get(),
        vec![
            "+C2", "+B2", "+A2", "SUBTREE 2", "+C1", "+B1", "+A1", "SUBTREE 1", "ROOT",
        ]
    );
}

// ---------------------------------------------------------------------------
// getOwner.test.ts
// ---------------------------------------------------------------------------

/// `should return current owner`
#[test]
fn should_return_current_owner() {
    create_root(|_| {
        let owner = get_owner();
        assert!(owner.is_some());
        create_effect(
            move |_| {
                assert!(get_owner().is_some());
                assert_ne!(get_owner(), owner);
            },
            move |_v: &(), _| {},
        );
    });
}

/// `should return parent scope from inside untrack`
#[test]
fn should_return_parent_scope_from_inside_untrack() {
    create_root(|_| {
        untrack(|| {
            assert!(get_owner().is_some());
        });
    });
}

// ---------------------------------------------------------------------------
// runWithOwner.test.ts
// ---------------------------------------------------------------------------

/// `should scope function to current scope`
///
/// Upstream pokes `owner._context` directly. Context is typed here, so the
/// ported test writes through `set_context` — same invariant: the captured
/// owner's context is visible when re-entered.
#[test]
fn should_scope_function_to_current_scope() {
    let ctx = create_context::<i32>();
    let mut captured: Option<Owner> = None;

    create_root(|_| {
        captured = get_owner();
        set_context(ctx, Some(1), None).unwrap();
    });

    run_with_owner(captured, || {
        assert_eq!(get_context(ctx, None).unwrap(), 1);
    });
}

/// `should return value`
#[test]
fn should_return_value() {
    assert_eq!(run_with_owner(None, || 100), 100);
}

/// `should handle errors`
#[test]
#[ignore = "blocked on boundaries.ts: needs createErrorBoundary to observe the routed error"]
fn should_handle_errors() {}
