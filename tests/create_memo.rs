//! Port of the synchronous subset of `tests/createMemo.test.ts`.
//!
//! That file is ~2 700 lines and mostly exercises async memos, `isPending`,
//! `latest` and loading boundaries — all out of scope. The synchronous tests
//! are ported in full; the rest are represented by the `#[ignore]` markers at
//! the bottom, one per upstream `describe` block, rather than one per test, to
//! keep the mapping readable. `PORTING-QUESTIONS.md` records the count.

mod common;

use common::{Counter, Spy};
use solid_rs::*;

/// `should store and return value on read`
#[test]
fn should_store_and_return_value_on_read() {
    let (x, _sx) = create_signal(1);
    let (y, _sy) = create_signal(1);
    let a = create_memo(move |_| x.get() + y.get());

    assert_eq!(a.get(), 2);
    flush();
    // Again, to ensure state is maintained.
    assert_eq!(a.get(), 2);
}

/// `should update when dependency is updated`
#[test]
fn should_update_when_dependency_is_updated() {
    let (x, set_x) = create_signal(1);
    let (y, set_y) = create_signal(1);
    let a = create_memo(move |_| x.get() + y.get());

    set_x.set(2);
    flush();
    assert_eq!(a.get(), 3);

    set_y.set(2);
    flush();
    assert_eq!(a.get(), 4);
}

/// `should update when deep dependency is updated`
#[test]
fn should_update_when_deep_dependency_is_updated() {
    let (x, set_x) = create_signal(1);
    let (y, _sy) = create_signal(1);
    let a = create_memo(move |_| x.get() + y.get());
    let b = create_memo(move |_| a.get());

    set_x.set(2);
    flush();
    assert_eq!(b.get(), 3);
}

/// `should update when deep computed dependency is updated`
#[test]
fn should_update_when_deep_computed_dependency_is_updated() {
    let (x, set_x) = create_signal(10);
    let (y, _sy) = create_signal(10);
    let a = create_memo(move |_| x.get() + y.get());
    let b = create_memo(move |_| a.get());
    let c = create_memo(move |_| b.get());

    set_x.set(20);
    flush();
    assert_eq!(c.get(), 30);
}

/// `should only re-compute when needed`
#[test]
fn should_only_re_compute_when_needed() {
    let computed = Spy::<i32>::new();
    let c = computed.clone();

    let (x, set_x) = create_signal(10);
    let (y, set_y) = create_signal(10);

    let a = create_memo(move |_| {
        let v = x.get() + y.get();
        c.call(v);
        v
    });
    assert_eq!(computed.times(), 1);
    assert!(computed.was_called_with(&20));

    a.get();
    assert_eq!(computed.times(), 1);

    set_x.set(20);
    assert_eq!(computed.times(), 1);
    flush();
    assert_eq!(computed.times(), 1);
    a.get();
    assert_eq!(computed.times(), 2);

    set_y.set(20);
    flush();
    assert_eq!(computed.times(), 2);
    a.get();
    assert_eq!(computed.times(), 3);
}

/// `should release ownerless memos after an untracked read`
#[test]
fn should_release_ownerless_memos_after_an_untracked_read() {
    let computed = Spy::<i32>::new();
    let c = computed.clone();
    let (x, set_x) = create_signal(0);
    let a = create_memo(move |_| {
        let v = x.get();
        c.call(v);
        v
    });

    assert_eq!(a.get(), 0);
    assert_eq!(computed.times(), 1);
    assert_eq!(a.get(), 0);
    assert_eq!(computed.times(), 2);

    set_x.set(1);
    flush();
    assert_eq!(computed.times(), 2);

    assert_eq!(a.get(), 1);
    assert_eq!(computed.times(), 3);
    assert_eq!(a.get(), 1);
    assert_eq!(computed.times(), 4);
}

/// `should recompute ownerless chains on direct read after suspension`
#[test]
fn should_recompute_ownerless_chains_on_direct_read_after_suspension() {
    let memo_a = Spy::<()>::new();
    let memo_b = Spy::<()>::new();
    let (ma, mb) = (memo_a.clone(), memo_b.clone());

    let (x, set_x) = create_signal(10);
    let (y, set_y) = create_signal(10);

    let a = create_memo(move |_| {
        ma.hit();
        x.get()
    });
    let b = create_memo(move |_| {
        mb.hit();
        y.get()
    });
    let c = create_memo(move |_| a.get() + b.get());

    assert_eq!(memo_a.times(), 1);
    assert_eq!(memo_b.times(), 1);
    assert_eq!(c.get(), 20);

    set_x.set(20);
    flush();

    assert_eq!(memo_a.times(), 1);
    assert_eq!(memo_b.times(), 1);
    assert_eq!(c.get(), 30);
    assert_eq!(memo_a.times(), 2);
    assert_eq!(memo_b.times(), 2);

    set_y.set(20);
    flush();

    assert_eq!(memo_a.times(), 2);
    assert_eq!(memo_b.times(), 2);
    assert_eq!(c.get(), 40);
    assert_eq!(memo_a.times(), 3);
    assert_eq!(memo_b.times(), 3);
}

/// `should keep observed ownerless memos subscribed`
#[test]
fn should_keep_observed_ownerless_memos_subscribed() {
    let memo_a = Spy::<()>::new();
    let memo_b = Spy::<()>::new();
    let (ma, mb) = (memo_a.clone(), memo_b.clone());
    let (x, set_x) = create_signal(10);
    let (y, set_y) = create_signal(10);
    let value = Counter::new();
    let v = value.clone();

    create_root(move |_| {
        let a = create_memo(move |_| {
            ma.hit();
            x.get()
        });
        let b = create_memo(move |_| {
            mb.hit();
            y.get()
        });
        let c = create_memo(move |_| a.get() + b.get());
        create_render_effect(
            move |_| c.get(),
            move |val: &i32, _| {
                v.reset();
                for _ in 0..*val {
                    v.inc();
                }
            },
        );
    });

    flush();
    assert_eq!(value.get(), 20);
    assert_eq!(memo_a.times(), 1);
    assert_eq!(memo_b.times(), 1);

    set_x.set(20);
    flush();
    assert_eq!(value.get(), 30);
    assert_eq!(memo_a.times(), 2);
    assert_eq!(memo_b.times(), 1);

    set_y.set(20);
    flush();
    assert_eq!(value.get(), 40);
    assert_eq!(memo_a.times(), 2);
    assert_eq!(memo_b.times(), 2);
}

/// `should discover new dependencies`
#[test]
fn should_discover_new_dependencies() {
    let (x, set_x) = create_signal(1);
    let (y, set_y) = create_signal(0);

    let c = create_memo(move |_| if x.get() != 0 { x.get() } else { y.get() });

    assert_eq!(c.get(), 1);

    set_x.set(0);
    flush();
    assert_eq!(c.get(), 0);

    set_y.set(10);
    flush();
    assert_eq!(c.get(), 10);
}

/// `should accept equals option`
#[test]
fn should_accept_equals_option() {
    let (x, set_x) = create_signal(0);

    // Skip even numbers.
    let a = create_memo_with(
        move |_| x.get(),
        Options::equals(|prev: &i32, next: &i32| prev + 1 == *next),
    );

    let effect_a = Spy::<()>::new();
    let e = effect_a.clone();
    create_root(move |_| create_effect(move |_| a.get(), move |_v: &i32, _| e.hit()));
    flush();

    assert_eq!(a.get(), 0);
    assert_eq!(effect_a.times(), 1);

    set_x.set(2);
    flush();
    assert_eq!(a.get(), 2);
    assert_eq!(effect_a.times(), 2);

    // no-change
    set_x.set(3);
    flush();
    assert_eq!(a.get(), 2);
    assert_eq!(effect_a.times(), 2);
}

/// `should ignore equals before memo initialization`
#[test]
fn should_ignore_equals_before_memo_initialization() {
    let (x, _set_x) = create_signal(1);
    let a = create_memo_with(move |_| x.get(), Options::equals(|_a: &i32, _b: &i32| true));
    assert_eq!(a.get(), 1);
}

/// `should route init errors through the boundary without a memo fallback`
#[test]
#[ignore = "blocked on boundaries.ts: needs createErrorBoundary"]
fn should_route_init_errors_through_the_boundary_without_a_memo_fallback() {}

// ---------------------------------------------------------------------------
// refresh() — synchronous cases from `describe("async compute")`
// ---------------------------------------------------------------------------

/// `should only refresh the targeted memo, not its upstream memos (#2691)`
#[test]
fn should_only_refresh_the_targeted_memo_not_its_upstream_memos() {
    let upstream_runs = Counter::new();
    let downstream_runs = Counter::new();
    let (u, d) = (upstream_runs.clone(), downstream_runs.clone());

    let downstream = create_root(move |_| {
        let (_x, set_x) = create_signal(0);
        let upstream = create_memo(move |_| {
            u.inc();
            set_x
        });
        let downstream = create_memo(move |_| {
            d.inc();
            upstream.get()
        });
        downstream.get(); // initial read
        downstream
    });
    flush();
    assert_eq!(upstream_runs.get(), 1);
    assert_eq!(downstream_runs.get(), 1);

    downstream.refresh();
    flush();
    assert_eq!(upstream_runs.get(), 1);
    assert_eq!(downstream_runs.get(), 2);
}

/// `should refresh explicit targets but not their deps (#2691)`
#[test]
fn should_refresh_explicit_targets_but_not_their_deps() {
    let a_runs = Counter::new();
    let b_runs = Counter::new();
    let a_dep_runs = Counter::new();
    let b_dep_runs = Counter::new();
    let (ar, br, adr, bdr) = (
        a_runs.clone(),
        b_runs.clone(),
        a_dep_runs.clone(),
        b_dep_runs.clone(),
    );

    let (a, b) = create_root(move |_| {
        let (s, _set) = create_signal(0);
        let a_dep = create_memo(move |_| {
            adr.inc();
            s.get()
        });
        let b_dep = create_memo(move |_| {
            bdr.inc();
            s.get()
        });
        let a = create_memo(move |_| {
            ar.inc();
            a_dep.get()
        });
        let b = create_memo(move |_| {
            br.inc();
            b_dep.get()
        });
        a.get();
        b.get();
        (a, b)
    });
    flush();
    assert_eq!(a_runs.get(), 1);
    assert_eq!(b_runs.get(), 1);
    assert_eq!(a_dep_runs.get(), 1);
    assert_eq!(b_dep_runs.get(), 1);

    a.refresh();
    b.refresh();
    flush();
    assert_eq!(a_runs.get(), 2);
    assert_eq!(b_runs.get(), 2);
    assert_eq!(a_dep_runs.get(), 1);
    assert_eq!(b_dep_runs.get(), 1);
}

/// `coalesces multiple refreshes of the same target in one flush`
#[test]
fn coalesces_multiple_refreshes_of_the_same_target_in_one_flush() {
    let runs = Counter::new();
    let r = runs.clone();

    let target = create_root(move |_| {
        let target = create_memo(move |_| r.inc());
        target.get();
        target
    });
    flush();
    assert_eq!(runs.get(), 1);

    target.refresh();
    target.refresh();
    target.refresh();
    flush();
    assert_eq!(runs.get(), 2);
}

/// `refresh() treats plain signal accessors as no-op targets`
#[test]
fn refresh_treats_plain_signal_accessors_as_no_op_targets() {
    let (count, _set) = create_signal(0);
    count.refresh();
    flush();
    assert_eq!(count.get(), 0);
}

/// `refresh() rejects unbranded function wrappers`
#[test]
#[ignore = "not representable: `refresh` is an inherent method on a typed handle, so an unbranded \
            wrapper cannot be passed — the upstream failure mode is a compile error here"]
fn refresh_rejects_unbranded_function_wrappers() {}

/// `refresh() rejects non-source objects`
#[test]
#[ignore = "not representable: see `refresh_rejects_unbranded_function_wrappers`"]
fn refresh_rejects_non_source_objects() {}

/// `refresh() rejects wrapped property reads`
#[test]
#[ignore = "not representable: see `refresh_rejects_unbranded_function_wrappers`"]
fn refresh_rejects_wrapped_property_reads() {}

/// `refresh() does not throw upstream pending reads from a plain memo (#2694)`
#[test]
#[ignore = "blocked on boundaries.ts: needs createLoadingBoundary"]
fn refresh_does_not_throw_upstream_pending_reads_from_a_plain_memo() {}

// ---------------------------------------------------------------------------
// Manual writes to a memo (#2692, #2745)
// ---------------------------------------------------------------------------

/// `should let a manual setSignal write to a memo win over a queued recompute (#2692)`
#[test]
fn manual_write_to_a_memo_wins_over_a_queued_recompute() {
    let (original, set_original, derived, set_derived) = create_root(|_| {
        let (o, set_o) = create_signal(0);
        let (d, set_d) = create_computed_signal(move |_| o.get());
        (o, set_o, d, set_d)
    });
    assert_eq!(derived.get(), 0);
    set_original.set(1);
    set_derived.set(2);
    flush();
    assert_eq!(original.get(), 1);
    assert_eq!(derived.get(), 2);
    // A later upstream change still recomputes the memo.
    set_original.set(3);
    flush();
    assert_eq!(derived.get(), 3);
}

/// `should let a manual setSignal write win even when an intermediate memo re-marks it (#2692)`
#[test]
fn manual_write_wins_even_when_an_intermediate_memo_re_marks_it() {
    let observed = Counter::new();
    let obs = observed.clone();

    let (set_a, set_c) = create_root(move |_| {
        let (a, s_a) = create_signal(0);
        let b = create_memo(move |_| a.get() * 10);
        let (c, s_c) = create_computed_signal(move |_| b.get());
        create_render_effect(
            move |_| c.get(),
            move |v: &i32, _| {
                obs.reset();
                for _ in 0..*v {
                    obs.inc();
                }
            },
        );
        (s_a, s_c)
    });
    flush();
    assert_eq!(observed.get(), 0);
    set_a.set(1);
    set_c.set(99);
    flush();
    assert_eq!(observed.get(), 99);
    set_a.set(2);
    flush();
    assert_eq!(observed.get(), 20);
}

/// `write trumps recompute regardless of order within a tick (#2692)`
#[test]
fn write_trumps_recompute_regardless_of_order_within_a_tick() {
    let (set_original, set_derived, derived) = create_root(|_| {
        let (original, set_o) = create_signal(0);
        let (d, set_d) = create_computed_signal(move |_| original.get());
        (set_o, set_d, d)
    });
    set_derived.set(2);
    set_original.set(1);
    flush();
    assert_eq!(derived.get(), 2);
}

/// `latest manual write wins on a memo within a tick (#2692)`
#[test]
fn latest_manual_write_wins_on_a_memo_within_a_tick() {
    let (set_original, set_derived, derived) = create_root(|_| {
        let (original, set_o) = create_signal(0);
        let (d, set_d) = create_computed_signal(move |_| original.get());
        (set_o, set_d, d)
    });
    set_original.set(1);
    set_derived.set(2);
    set_derived.set(3);
    flush();
    assert_eq!(derived.get(), 3);
}

/// `derived signal resumes tracking after repeated same manual writes (#2745)`
#[test]
fn derived_signal_resumes_tracking_after_repeated_same_manual_writes() {
    let observed = Counter::new();
    let obs = observed.clone();

    let (set_original, set_derived) = create_root(move |_| {
        let (original, set_o) = create_signal(0);
        let (derived, set_d) = create_computed_signal(move |_| original.get());
        create_render_effect(
            move |_| derived.get(),
            move |v: &i32, _| {
                obs.reset();
                for _ in 0..*v {
                    obs.inc();
                }
            },
        );
        (set_o, set_d)
    });

    flush();
    assert_eq!(observed.get(), 0);

    set_derived.set(42);
    flush();
    assert_eq!(observed.get(), 42);

    set_derived.set(42);
    flush();
    assert_eq!(observed.get(), 42);

    set_original.set(1);
    flush();
    assert_eq!(observed.get(), 1);
}

/// `same-value manual write still trumps recompute within a tick (#2692)`
#[test]
fn same_value_manual_write_still_trumps_recompute_within_a_tick() {
    let (set_original, set_derived, derived) = create_root(|_| {
        let (original, set_o) = create_signal(0);
        let (d, set_d) = create_computed_signal(move |_| original.get());
        (set_o, set_d, d)
    });
    set_original.set(1);
    set_derived.set(0);
    flush();
    assert_eq!(derived.get(), 0);
}

/// `manual memo writes win over refresh within the same tick (#2692)`
#[test]
fn manual_memo_writes_win_over_refresh_within_the_same_tick() {
    let (set_original, set_derived, derived) = create_root(|_| {
        let (original, set_o) = create_signal(0);
        let (d, set_d) = create_computed_signal(move |_| original.get());
        (set_o, set_d, d)
    });
    set_original.set(1);
    set_derived.set(2);
    derived.refresh();
    flush();
    assert_eq!(derived.get(), 2);

    derived.refresh();
    set_derived.set(3);
    flush();
    assert_eq!(derived.get(), 3);
}

/// `should propagate manual setSignal write to subscribers when upstream also changed (#2692)`
#[test]
fn should_propagate_manual_write_to_subscribers_when_upstream_also_changed() {
    let observed = Counter::new();
    let obs = observed.clone();

    let (set_original, set_derived) = create_root(move |_| {
        let (original, set_o) = create_signal(0);
        let (derived, set_d) = create_computed_signal(move |_| original.get());
        create_render_effect(
            move |_| derived.get(),
            move |v: &i32, _| {
                obs.reset();
                for _ in 0..*v {
                    obs.inc();
                }
            },
        );
        (set_o, set_d)
    });
    flush();
    assert_eq!(observed.get(), 0);
    set_original.set(1);
    set_derived.set(2);
    flush();
    assert_eq!(observed.get(), 2);
}

// ---------------------------------------------------------------------------
// Lazy memos
// ---------------------------------------------------------------------------

/// `should compute lazy memo when first read inside another computation`
#[test]
fn should_compute_lazy_memo_when_first_read_inside_another_computation() {
    let (x, set_x) = create_signal(0);
    let inner_runs = Counter::new();
    let r = inner_runs.clone();

    let probe = inner_runs.clone();
    let outer = create_root(move |_| {
        let lazy = create_memo_with(
            move |_| {
                r.inc();
                x.get() + 1
            },
            Options::default().lazy(true),
        );
        assert_eq!(probe.get(), 0, "lazy memo has not computed yet");

        let outer = create_memo(move |_| lazy.get() * 2);
        assert_eq!(probe.get(), 1, "the outer memo's read drove it");
        assert_eq!(outer.get(), 2);
        outer
    });

    set_x.set(5);
    flush();
    assert_eq!(inner_runs.get(), 2);
    assert_eq!(outer.get(), 12);
}

/// `should compute nested lazy memos when first read inside another computation`
#[test]
fn should_compute_nested_lazy_memos_when_first_read_inside_another_computation() {
    let (x, set_x) = create_signal(0);

    let eager = create_root(move |_| {
        let lazy_outer = create_memo_with(move |_| x.get() + 1, Options::default().lazy(true));
        let lazy_inner =
            create_memo_with(move |_| lazy_outer.get() * 10, Options::default().lazy(true));
        let eager = create_memo(move |_| lazy_inner.get() + 100);
        assert_eq!(eager.get(), 110);
        eager
    });

    set_x.set(5);
    flush();
    assert_eq!(eager.get(), 160);
}

// ---------------------------------------------------------------------------
// Out-of-scope blocks, one marker per upstream `describe` / theme.
// ---------------------------------------------------------------------------

/// `describe("async compute")` — the async memo lifecycle.
#[test]
#[ignore = "phase 2: async memos work (see tests/async_memo.rs), but this block's waterfall, \
            transition-entanglement and settle-ordering cases need boundaries.ts + lanes.ts"]
fn describe_async_compute() {}

/// `describe("isPending and latest with async upstream and downstream")`
#[test]
#[ignore = "phase 2: the isPending/latest ladder is ported, but this block needs the override \
            branch (optimistic.ts) and lane checks (lanes.ts)"]
fn describe_is_pending_and_latest() {}

/// `should dispose all sibling children when async memo resolves to falsy value`
#[test]
#[ignore = "blocked on boundaries.ts: the sibling-disposal path runs through a boundary"]
fn should_dispose_all_sibling_children_when_async_memo_resolves_to_falsy_value() {}
