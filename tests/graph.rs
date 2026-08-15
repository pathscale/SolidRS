//! Port of `tests/graph.test.ts`.
//!
//! Upstream note: derived from
//! <https://github.com/preactjs/signals/blob/17c0155/packages/core/test/signal.test.tsx#L1383>
//!
//! Every test here is synchronous, so the whole file ports.

mod common;

use common::{Counter, Order, Spy};
use solid_rs::*;

/// `should drop X->B->X updates`
#[test]
fn should_drop_x_b_x_updates() {
    //     X
    //   / |
    //  A  |
    //   \ |
    //     B
    //     |
    //     C
    let compute = Spy::<()>::new();
    let c = compute.clone();

    let (set_x, cc) = create_root(|_| {
        let (x, set_x) = create_signal(2);
        let a = create_memo(move |_| x.get() - 1);
        let b = create_memo(move |_| x.get() + a.get());
        let cc = create_memo(move |_| {
            c.hit();
            format!("c: {}", b.get())
        });
        (set_x, cc)
    });

    assert_eq!(cc.get(), "c: 3");
    assert_eq!(compute.times(), 1);
    compute.reset();

    set_x.set(4);
    flush();
    assert_eq!(compute.times(), 1);
}

/// `should only update every signal once (diamond graph)`
#[test]
fn should_only_update_every_signal_once_diamond_graph() {
    //     X
    //   /   \
    //  A     B
    //   \   /
    //     C
    let (x, set_x) = create_signal("a".to_string());
    let a = create_memo(move |_| x.get());
    let b = create_memo(move |_| x.get());

    let spy = Spy::<()>::new();
    let s = spy.clone();
    let c = create_memo(move |_| {
        s.hit();
        format!("{} {}", a.get(), b.get())
    });

    assert_eq!(c.get(), "a a");
    assert_eq!(spy.times(), 1);

    set_x.set("aa".to_string());
    flush();
    assert_eq!(c.get(), "aa aa");
    assert_eq!(spy.times(), 2);
}

/// `should only update every signal once (diamond graph + tail)`
#[test]
fn should_only_update_every_signal_once_diamond_graph_plus_tail() {
    let (x, set_x) = create_signal("a".to_string());
    let a = create_memo(move |_| x.get());
    let b = create_memo(move |_| x.get());
    let c = create_memo(move |_| format!("{} {}", a.get(), b.get()));

    let spy = Spy::<()>::new();
    let s = spy.clone();
    let d = create_memo(move |_| {
        s.hit();
        c.get()
    });

    assert_eq!(d.get(), "a a");
    assert_eq!(spy.times(), 1);

    set_x.set("aa".to_string());
    flush();
    assert_eq!(d.get(), "aa aa");
    assert_eq!(spy.times(), 2);
}

/// `should bail out if result is the same`
#[test]
fn should_bail_out_if_result_is_the_same() {
    // Bail out if the value of "A" never changes: X->A->B
    let spy = Spy::<()>::new();
    let s = spy.clone();

    let (set_x, b) = create_root(|_| {
        let (x, set_x) = create_signal("a".to_string());
        let a = create_memo(move |_| {
            x.get();
            "foo".to_string()
        });
        let b = create_memo(move |_| {
            s.hit();
            a.get()
        });
        (set_x, b)
    });

    assert_eq!(b.get(), "foo");
    assert_eq!(spy.times(), 1);

    set_x.set("aa".to_string());
    flush();
    assert_eq!(b.get(), "foo");
    assert_eq!(spy.times(), 1);
}

/// `should only update every signal once (jagged diamond graph + tails)`
#[test]
fn should_only_update_every_signal_once_jagged_diamond_graph_plus_tails() {
    //     X
    //   /   \
    //  A     B
    //  |     |
    //  |     C
    //   \   /
    //     D
    //   /   \
    //  E     F
    let d_spy = Spy::<()>::new();
    let e_spy = Spy::<()>::new();
    let f_spy = Spy::<()>::new();
    let (ds, es, fs) = (d_spy.clone(), e_spy.clone(), f_spy.clone());

    let (set_x, d, e, f) = create_root(|_| {
        let (x, set_x) = create_signal("a".to_string());
        let a = create_memo(move |_| x.get());
        let b = create_memo(move |_| x.get());
        let c = create_memo(move |_| b.get());
        let d = create_memo(move |_| {
            ds.hit();
            format!("{} {}", a.get(), c.get())
        });
        let e = create_memo(move |_| {
            es.hit();
            d.get()
        });
        let f = create_memo(move |_| {
            fs.hit();
            d.get()
        });
        (set_x, d, e, f)
    });

    assert_eq!(e.get(), "a a");
    assert_eq!(e_spy.times(), 1);
    assert_eq!(f.get(), "a a");
    assert_eq!(f_spy.times(), 1);

    set_x.set("b".to_string());
    flush();

    assert_eq!(d.get(), "b b");
    assert_eq!(d_spy.times(), 2);
    assert_eq!(e.get(), "b b");
    assert_eq!(e_spy.times(), 2);
    assert_eq!(f.get(), "b b");
    assert_eq!(f_spy.times(), 2);

    set_x.set("c".to_string());
    flush();

    assert_eq!(d.get(), "c c");
    assert_eq!(d_spy.times(), 3);
    assert_eq!(e.get(), "c c");
    assert_eq!(e_spy.times(), 3);
    assert_eq!(f.get(), "c c");
    assert_eq!(f_spy.times(), 3);
}

/// `should ensure subs update even if one dep is static`
#[test]
fn should_ensure_subs_update_even_if_one_dep_is_static() {
    //     X
    //   /   \
    //  A     *B <- returns the same value every time
    //   \   /
    //     C
    let (x, set_x) = create_signal("a".to_string());
    let a = create_memo(move |_| x.get());
    let b = create_memo(move |_| {
        x.get();
        "c".to_string()
    });

    let spy = Spy::<()>::new();
    let s = spy.clone();
    let c = create_memo(move |_| {
        s.hit();
        format!("{} {}", a.get(), b.get())
    });

    assert_eq!(c.get(), "a c");

    set_x.set("aa".to_string());
    flush();

    assert_eq!(c.get(), "aa c");
    assert_eq!(spy.times(), 2);
}

/// `should ensure subs update even if two deps mark it clean`
#[test]
fn should_ensure_subs_update_even_if_two_deps_mark_it_clean() {
    // Both "C" and "D" always return the same value, but "E" must still update
    // because "X" marked it.
    //     X
    //   / | \
    //  B *C *D
    //   \ | /
    //     E
    let (x, set_x) = create_signal("a".to_string());
    let b = create_memo(move |_| x.get());
    let c = create_memo(move |_| {
        x.get();
        "c".to_string()
    });
    let d = create_memo(move |_| {
        x.get();
        "d".to_string()
    });

    let spy = Spy::<()>::new();
    let s = spy.clone();
    let e = create_memo(move |_| {
        s.hit();
        format!("{} {} {}", b.get(), c.get(), d.get())
    });

    assert_eq!(e.get(), "a c d");

    set_x.set("aa".to_string());
    flush();

    assert_eq!(e.get(), "aa c d");
    assert_eq!(spy.times(), 2);
}

/// `propagates in topological order`
#[test]
fn propagates_in_topological_order() {
    //     c1
    //    /  \
    //   b1   b2
    //    \  /
    //     a1
    let seq = Order::new();
    let (s1, s2, s3) = (seq.clone(), seq.clone(), seq.clone());

    let (a1, set_a1) = create_signal(false);
    let b1 = create_memo_with(
        move |_| {
            a1.get();
            s1.push("b1");
        },
        Options::never_equal(),
    );
    let b2 = create_memo_with(
        move |_| {
            a1.get();
            s2.push("b2");
        },
        Options::never_equal(),
    );
    let _c1 = create_memo_with(
        move |_| {
            b1.get();
            b2.get();
            s3.push("c1");
        },
        Options::never_equal(),
    );
    seq.reset();
    set_a1.set(true);
    flush();
    assert_eq!(seq.get(), "b1b2c1");
}

/// `only propagates once with linear convergences`
#[test]
fn only_propagates_once_with_linear_convergences() {
    //         d
    // +---+---+---+---+
    // f1  f2  f3  f4  f5
    // +---+---+---+---+
    //         g
    let (d, set_d) = create_signal(0);
    let f1 = create_memo(move |_| d.get());
    let f2 = create_memo(move |_| d.get());
    let f3 = create_memo(move |_| d.get());
    let f4 = create_memo(move |_| d.get());
    let f5 = create_memo(move |_| d.get());

    let gcount = Counter::new();
    let g = gcount.clone();
    let gm = create_memo(move |_| {
        g.inc();
        f1.get() + f2.get() + f3.get() + f4.get() + f5.get()
    });
    let _ = gm;
    gcount.reset();
    set_d.set(1);
    flush();
    assert_eq!(gcount.get(), 1);
}

/// `only propagates once with exponential convergence`
#[test]
fn only_propagates_once_with_exponential_convergence() {
    let (d, set_d) = create_signal(0);
    let f1 = create_memo(move |_| d.get());
    let f2 = create_memo(move |_| d.get());
    let f3 = create_memo(move |_| d.get());
    let g1 = create_memo(move |_| f1.get() + f2.get() + f3.get());
    let g2 = create_memo(move |_| f1.get() + f2.get() + f3.get());
    let g3 = create_memo(move |_| f1.get() + f2.get() + f3.get());

    let hcount = Counter::new();
    let h = hcount.clone();
    let hm = create_memo(move |_| {
        h.inc();
        g1.get() + g2.get() + g3.get()
    });
    let _ = hm;
    hcount.reset();
    set_d.set(1);
    flush();
    assert_eq!(hcount.get(), 1);
}

/// `does not trigger downstream computations unless changed`
#[test]
fn does_not_trigger_downstream_computations_unless_changed() {
    let (s1, set) = create_signal_with(1, Options::never_equal());
    let order = Order::new();
    let (o1, o2) = (order.clone(), order.clone());

    let t1 = create_memo(move |_| {
        o1.push("t1");
        s1.get()
    });
    let t2 = create_memo(move |_| {
        o2.push("c1");
        t1.get();
    });
    let _ = t2;

    assert_eq!(order.get(), "t1c1");
    order.reset();
    set.set(1);
    flush();
    assert_eq!(order.get(), "t1");
    order.reset();
    set.set(2);
    flush();
    assert_eq!(order.get(), "t1c1");
}

/// `applies updates to changed dependees in same order as createMemo`
#[test]
fn applies_updates_to_changed_dependees_in_same_order_as_create_memo() {
    let (s1, set) = create_signal(0);
    let order = Order::new();
    let (o1, o2, o3) = (order.clone(), order.clone(), order.clone());

    let t1 = create_memo(move |_| {
        o1.push("t1");
        s1.get() == 0
    });
    let t2 = create_memo(move |_| {
        o2.push("c1");
        s1.get()
    });
    let t3 = create_memo(move |_| {
        o3.push("c2");
        t1.get()
    });
    let _ = (t2, t3);

    assert_eq!(order.get(), "t1c1c2");
    order.reset();
    set.set(1);
    flush();
    assert_eq!(order.get(), "t1c1c2");
}

/// `updates downstream pending computations`
#[test]
fn updates_downstream_pending_computations() {
    let order = Order::new();
    let (o1, o2, o3, o4) = (
        order.clone(),
        order.clone(),
        order.clone(),
        order.clone(),
    );

    let (set, t2, t3) = create_root(|_| {
        let (s1, set) = create_signal(0);
        let (s2, _set2) = create_signal(0);
        let t1 = create_memo(move |_| {
            o1.push("t1");
            s1.get() == 0
        });
        let t2 = create_memo(move |_| {
            o2.push("c1");
            s1.get()
        });
        let t3 = create_memo(move |_| {
            o3.push("c2");
            t1.get();
            let o = o4.clone();
            create_memo(move |_| {
                o.push("c2_1");
                s2.get()
            })
        });
        (set, t2, t3)
    });
    order.reset();

    set.set(1);
    flush();
    t2.get();
    t3.get().get();
    assert_eq!(order.get(), "t1c1c2c2_1");
}

mod with_changing_dependencies {
    use super::*;

    struct Fixture {
        set_i: WriteSignal<bool>,
        set_t: WriteSignal<i32>,
        set_e: WriteSignal<i32>,
        f: Memo<i32>,
        fevals: Counter,
    }

    fn init() -> Fixture {
        create_root(|_| {
            let (i, set_i) = create_signal(true);
            let (t, set_t) = create_signal(1);
            let (e, set_e) = create_signal(2);
            let fevals = Counter::new();
            let c = fevals.clone();
            let f = create_memo(move |_| {
                c.inc();
                if i.get() {
                    t.get()
                } else {
                    e.get()
                }
            });
            f.get();
            fevals.reset();
            Fixture {
                set_i,
                set_t,
                set_e,
                f,
                fevals,
            }
        })
    }

    /// `updates on active dependencies`
    #[test]
    fn updates_on_active_dependencies() {
        let fx = init();
        fx.set_t.set(5);
        flush();
        assert_eq!(fx.f.get(), 5);
        assert_eq!(fx.fevals.get(), 1);
    }

    /// `does not update on inactive dependencies`
    #[test]
    fn does_not_update_on_inactive_dependencies() {
        let fx = init();
        fx.set_e.set(5);
        flush();
        assert_eq!(fx.f.get(), 1);
        assert_eq!(fx.fevals.get(), 0);
    }

    /// `deactivates obsolete dependencies`
    #[test]
    fn deactivates_obsolete_dependencies() {
        let fx = init();
        fx.set_i.set(false);
        flush();
        fx.fevals.reset();
        fx.set_t.set(5);
        flush();
        assert_eq!(fx.fevals.get(), 0);
    }

    /// `activates new dependencies`
    #[test]
    fn activates_new_dependencies() {
        let fx = init();
        fx.set_i.set(false);
        flush();
        fx.fevals.reset();
        fx.set_e.set(5);
        flush();
        assert_eq!(fx.fevals.get(), 1);
    }

    /// `ensures that new dependencies are updated before dependee`
    #[test]
    fn ensures_that_new_dependencies_are_updated_before_dependee() {
        let order = Order::new();
        let (o1, o2, o3) = (order.clone(), order.clone(), order.clone());

        let (set_a, c) = create_root(|_| {
            let (a, set_a) = create_signal(0);
            let b = create_memo(move |_| {
                o1.push("b");
                a.get() + 1
            });
            // Upstream declares `e` after `c` and relies on `var` hoisting.
            // Creation order is exactly what this test measures, so it is
            // preserved with a forward slot instead of being reordered.
            let e_slot: std::rc::Rc<std::cell::Cell<Option<Memo<i32>>>> =
                std::rc::Rc::new(std::cell::Cell::new(None));
            let e_ref = e_slot.clone();
            let c = create_memo(move |_| {
                o2.push("c");
                let check = b.get();
                if check != 0 {
                    check
                } else {
                    e_ref.get().expect("e is created before c re-runs").get()
                }
            });
            let d = create_memo(move |_| a.get());
            let e = create_memo(move |_| {
                o3.push("d");
                d.get() + 10
            });
            e_slot.set(Some(e));
            (set_a, c)
        });

        assert_eq!(order.get(), "bcd");
        order.reset();

        set_a.set(-1);
        flush();
        assert_eq!(order.get(), "bcd");
        assert_eq!(c.get(), 9);

        order.reset();
        set_a.set(0);
        flush();
        assert_eq!(order.get(), "bdc");
        assert_eq!(c.get(), 1);
    }
}

/// `does not update subsequent pending computations after stale invocations`
#[test]
fn does_not_update_subsequent_pending_computations_after_stale_invocations() {
    //         s1
    //   +---+---+---+
    //  t1  t2  c1  t3
    //   \           /
    //         c3
    let (s1, set1) = create_signal(1);
    let (s2, set2) = create_signal(false);
    let count = Counter::new();
    let c = count.clone();

    let t1 = create_memo(move |_| s1.get() > 0);
    let t2 = create_memo(move |_| s1.get() > 0);
    let c1 = create_memo(move |_| s1.get());
    let t3 = create_memo(move |_| {
        let a = s1.get();
        let b = s2.get();
        a > 0 && b
    });
    let c3 = create_memo(move |_| {
        t1.get();
        t2.get();
        c1.get();
        t3.get();
        c.inc()
    });
    let _ = c3;

    set2.set(true);
    flush();
    assert_eq!(count.get(), 2);
    set1.set(2);
    flush();
    assert_eq!(count.get(), 3);
}

/// `evaluates stale computations before dependees when trackers stay unchanged`
#[test]
fn evaluates_stale_computations_before_dependees_when_trackers_stay_unchanged() {
    let (s1, set) = create_signal_with(1, Options::never_equal());
    let order = Order::new();
    let (o1, o2, o3, o4) = (
        order.clone(),
        order.clone(),
        order.clone(),
        order.clone(),
    );

    let t1 = create_memo(move |_| {
        o1.push("t1");
        s1.get() > 2
    });
    let t2 = create_memo(move |_| {
        o2.push("t2");
        s1.get() > 2
    });
    let c1 = create_memo_with(
        move |_| {
            o3.push("c1");
            s1.get();
        },
        Options::never_equal(),
    );
    let c2 = create_memo(move |_| {
        o4.push("c2");
        t1.get();
        t2.get();
        c1.get();
    });
    let _ = c2;

    order.reset();
    set.set(1);
    flush();
    assert_eq!(order.get(), "t1t2c1c2");
    order.reset();
    set.set(3);
    flush();
    assert_eq!(order.get(), "t1t2c1c2");
}

/// `correctly marks downstream computations as stale on change`
#[test]
fn correctly_marks_downstream_computations_as_stale_on_change() {
    let (s1, set) = create_signal(1);
    let order = Order::new();
    let (o1, o2, o3, o4) = (
        order.clone(),
        order.clone(),
        order.clone(),
        order.clone(),
    );

    let t1 = create_memo(move |_| {
        o1.push("t1");
        s1.get()
    });
    let c1 = create_memo(move |_| {
        o2.push("c1");
        t1.get()
    });
    let c2 = create_memo(move |_| {
        o3.push("c2");
        c1.get()
    });
    let c3 = create_memo(move |_| {
        o4.push("c3");
        c2.get()
    });
    let _ = c3;

    order.reset();
    set.set(2);
    flush();
    assert_eq!(order.get(), "t1c1c2c3");
}
