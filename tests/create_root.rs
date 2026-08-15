//! Port of `tests/createRoot.test.ts`.

mod common;

use common::{Counter, Log, Spy};
use solid_rs::*;
use std::cell::Cell;
use std::rc::Rc;

/// `should dispose of inner computations`
#[test]
fn should_dispose_of_inner_computations() {
    let memo_spy = Spy::<()>::new();
    let m = memo_spy.clone();

    let set_x = create_root(move |dispose| {
        let (x, set_x) = create_signal(10);
        let _y = create_memo(move |_| {
            m.hit();
            x.get() + 10
        });
        dispose.dispose();
        set_x
    });

    assert_eq!(memo_spy.times(), 1);
    flush();

    set_x.set(50);
    flush();
    assert_eq!(memo_spy.times(), 1);
}

/// `should not resurrect a dependency-free computation queued before disposal (#2983)`
#[test]
fn should_not_resurrect_a_dependency_free_computation_queued_before_disposal() {
    // Owner disposal must pull queued children out of the scheduler heap even
    // when they have no reactive dependencies. A dependency-free memo queued by
    // refresh() and left in the heap would be recomputed by the next flush, and
    // recompute() rewriting flags would clear DISPOSED — resurrecting the node.
    let runs = Counter::new();
    let cleanups = Counter::new();
    let (r, c) = (runs.clone(), cleanups.clone());

    let (read, dispose) = create_root(move |d| {
        let read = create_memo(move |_| {
            let n = r.inc();
            let c = c.clone();
            on_cleanup(move || {
                c.inc();
            });
            n
        });
        (read, d)
    });

    read.refresh(); // queue the dependency-free memo
    dispose.dispose(); // dispose before the queued flush
    flush();

    assert_eq!(runs.get(), 1);
    assert_eq!(cleanups.get(), 1);
}

/// `should return result`
#[test]
fn should_return_result() {
    let result = create_root(|dispose| {
        dispose.dispose();
        10
    });
    assert_eq!(result, 10);
}

/// `should create new tracking scope`
#[test]
fn should_create_new_tracking_scope() {
    let (x, set_x) = create_signal(0);
    let effect = Spy::<(i32, Option<i32>)>::new();
    let e = effect.clone();

    let stop = create_root(move |dispose| {
        create_effect(
            move |_| {
                x.get();
                let e = e.clone();
                create_root(move |_| {
                    create_effect(
                        move |_| x.get(),
                        move |v: &i32, p: Option<&i32>| e.call((*v, p.copied())),
                    )
                });
            },
            move |_v: &(), _| {},
        );
        dispose
    });
    flush();

    assert!(effect.was_called_with(&(0, None)));
    assert_eq!(effect.times(), 1);

    stop.dispose();

    set_x.set(10);
    flush();
    assert!(!effect.was_called_with(&(10, Some(0))));
    assert_eq!(effect.times(), 1);
}

/// `should not be reactive`
#[test]
fn should_not_be_reactive() {
    let root = Spy::<()>::new();
    let r = root.clone();

    let set_x = create_root(move |_| {
        let (x, set_x) = create_signal(0);
        x.get();
        r.hit();
        set_x
    });

    assert_eq!(root.times(), 1);

    set_x.set(1);
    flush();
    assert_eq!(root.times(), 1);
}

/// `should hold parent tracking`
#[test]
fn should_hold_parent_tracking() {
    create_root(|_| {
        let parent = get_owner();
        create_root(move |_| {
            assert_eq!(get_owner().and_then(|o| o.parent()), parent);
        });
    });
}

/// `should not observe`
///
/// Upstream asserts the *structural absence* of `_subs` / `_deps` on a Root
/// object. Every node here carries those fields, so the ported assertion is
/// the equivalent invariant: a root neither subscribes nor is subscribed to.
#[test]
fn should_not_observe() {
    let (x, _set_x) = create_signal(0);
    create_root(move |_| {
        x.get();
        let owner = get_owner().expect("inside a root");
        assert!(!debug::has_dependencies(owner_node(owner)));
        assert_eq!(debug::subscriber_count(owner_node(owner)), 0);
    });
}

fn owner_node(o: Owner) -> NodeId {
    // `Owner` deliberately does not expose its key in the public API; the
    // ported structural assertions reach it through the round-trip that the
    // debug module is built for.
    o.node_id()
}

/// `should not throw if dispose called during active disposal process`
#[test]
fn should_not_throw_if_dispose_called_during_active_disposal_process() {
    create_root(|dispose| {
        let d = dispose;
        on_cleanup(move || d.dispose());
        dispose.dispose();
    });
}

/// `should not generate ids if no id is provided`
#[test]
fn should_not_generate_ids_if_no_id_is_provided() {
    let owner_id: Rc<Cell<Option<Rc<str>>>> = Rc::new(Cell::new(None));
    let memo_id: Rc<Cell<Option<Rc<str>>>> = Rc::new(Cell::new(None));
    let (o, m) = (owner_id.clone(), memo_id.clone());

    create_root(move |_| {
        o.set(get_owner().and_then(|x| x.hydration_id()));
        let c = create_memo(move |_| {
            m.set(get_owner().and_then(|x| x.hydration_id()));
        });
        c.get();
    });

    assert!(owner_id.take().is_none());
    assert!(memo_id.take().is_none());
}

/// `should generate ids if id is provided`
#[test]
fn should_generate_ids_if_id_is_provided() {
    let captured: Log<(&'static str, String)> = Log::new();
    let (c1, c2, c3) = (captured.clone(), captured.clone(), captured.clone());

    create_root_with(
        move |_| {
            c1.push(("o", id_of_owner()));
            let inner = c2.clone();
            let memo = create_memo(move |_| {
                inner.push(("m", id_of_owner()));
                let owner = get_owner().expect("owner");
                inner.push(("c", get_next_child_id(owner).unwrap().to_string()));
                let inner2 = inner.clone();
                create_memo(move |_| {
                    inner2.push(("m2", id_of_owner()));
                    let owner = get_owner().expect("owner");
                    inner2.push(("c2", get_next_child_id(owner).unwrap().to_string()));
                    inner2.push(("c3", get_next_child_id(owner).unwrap().to_string()));
                })
            });
            create_render_effect(
                move |_| {
                    c3.push(("r", id_of_owner()));
                    memo.get().get();
                },
                move |_v: &(), _| {},
            );
        },
        Some("".into()),
        false,
    );

    let got = |key: &str| -> String {
        captured
            .get()
            .into_iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
            .unwrap_or_else(|| panic!("missing {key}"))
    };

    assert_eq!(got("o"), "");
    assert_eq!(got("m"), "0");
    assert_eq!(got("c"), "00");
    assert_eq!(got("m2"), "01");
    assert_eq!(got("r"), "1");
    assert_eq!(got("c2"), "010");
    assert_eq!(got("c3"), "011");
}

fn id_of_owner() -> String {
    get_owner()
        .and_then(|o| o.hydration_id())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// `should reset child ids when computations rerun`
#[test]
fn should_reset_child_ids_when_computations_rerun() {
    let (x, set_x) = create_signal(0);
    let ids: Log<Vec<String>> = Log::new();
    let i = ids.clone();

    create_root_with(
        move |_| {
            create_memo(move |_| {
                x.get();
                let run: Vec<String> = vec![
                    create_memo(|_| id_of_owner()).get(),
                    create_memo(|_| id_of_owner()).get(),
                ];
                i.push(run);
            });
        },
        Some("".into()),
        false,
    );

    assert_eq!(ids.len(), 1);
    assert_eq!(ids.get()[0], vec!["00", "01"]);

    set_x.set(1);
    flush();

    assert_eq!(ids.len(), 2);
    assert_eq!(ids.get()[1], vec!["00", "01"]);
}

// ---------------------------------------------------------------------------
// `describe("transparent owners")`
// ---------------------------------------------------------------------------

/// `should not consume a _childCount slot from parent`
#[test]
fn transparent_should_not_consume_a_child_count_slot_from_parent() {
    let sibling: Rc<Cell<Option<Rc<str>>>> = Rc::new(Cell::new(None));
    let s = sibling.clone();

    create_root_with(
        move |_| {
            create_root_with(|_| {}, None, true);
            let memo = create_memo(|_| id_of_owner());
            s.set(Some(memo.get().into()));
        },
        Some("".into()),
        false,
    );

    assert_eq!(&*sibling.take().unwrap(), "0");
}

/// `should delegate child IDs to the nearest non-transparent ancestor`
#[test]
fn transparent_should_delegate_child_ids_to_nearest_non_transparent_ancestor() {
    let out: Log<String> = Log::new();
    let o = out.clone();

    create_root_with(
        move |_| {
            create_root_with(
                move |_| {
                    o.push(create_memo(|_| id_of_owner()).get());
                    o.push(create_memo(|_| id_of_owner()).get());
                },
                None,
                true,
            );
        },
        Some("".into()),
        false,
    );

    assert_eq!(out.get(), vec!["0", "1"]);
}

/// `should handle nested transparent owners delegating to non-transparent ancestor`
#[test]
fn transparent_should_handle_nested_transparent_owners() {
    let out: Log<String> = Log::new();
    let o = out.clone();

    create_root_with(
        move |_| {
            create_root_with(
                move |_| {
                    create_root_with(
                        move |_| o.push(create_memo(|_| id_of_owner()).get()),
                        None,
                        true,
                    );
                },
                None,
                true,
            );
        },
        Some("".into()),
        false,
    );

    assert_eq!(out.get(), vec!["0"]);
}

/// `should not consume _childCount with transparent computed`
#[test]
fn transparent_computed_should_not_consume_child_count() {
    let out: Log<String> = Log::new();
    let o = out.clone();

    create_root_with(
        move |_| {
            let transparent_memo = create_memo_with(|_| (), Options::default().transparent(true));
            transparent_memo.get();
            o.push(create_memo(|_| id_of_owner()).get());
        },
        Some("".into()),
        false,
    );

    assert_eq!(out.get(), vec!["0"]);
}

/// `should delegate child IDs through transparent computed`
#[test]
fn transparent_computed_should_delegate_child_ids() {
    let out: Log<String> = Log::new();
    let o = out.clone();

    create_root_with(
        move |_| {
            let transparent_memo = create_memo_with(
                move |_| {
                    o.push(create_memo(|_| id_of_owner()).get());
                    o.push(create_memo(|_| id_of_owner()).get());
                },
                Options::never_equal().transparent(true),
            );
            transparent_memo.get();
        },
        Some("".into()),
        false,
    );

    assert_eq!(out.get(), vec!["0", "1"]);
}

/// `should produce identical IDs with and without a transparent wrapper`
#[test]
fn transparent_produces_identical_ids_with_and_without_wrapper() {
    let without: Log<String> = Log::new();
    let with: Log<String> = Log::new();
    let (wo, wi) = (without.clone(), with.clone());

    create_root_with(
        move |_| {
            wo.push(create_memo(|_| id_of_owner()).get());
            wo.push(create_memo(|_| id_of_owner()).get());
        },
        Some("".into()),
        false,
    );

    create_root_with(
        move |_| {
            create_root_with(
                move |_| {
                    wi.push(create_memo(|_| id_of_owner()).get());
                    wi.push(create_memo(|_| id_of_owner()).get());
                },
                None,
                true,
            );
        },
        Some("".into()),
        false,
    );

    assert_eq!(with.get(), without.get());
}

/// `should inherit parent id on the transparent owner itself`
#[test]
fn transparent_owner_inherits_parent_id() {
    let out: Log<(&'static str, String)> = Log::new();
    let o = out.clone();

    create_root_with(
        move |_| {
            o.push(("parent", id_of_owner()));
            let o2 = o.clone();
            create_root_with(move |_| o2.push(("transparent", id_of_owner())), None, true);
        },
        Some("".into()),
        false,
    );

    assert_eq!(out.get(), vec![("parent", "".into()), ("transparent", "".into())]);
}

// ---------------------------------------------------------------------------
// `describe("peekNextChildId")`
// ---------------------------------------------------------------------------

/// `should return the next child id without incrementing the counter`
#[test]
fn peek_returns_next_child_id_without_incrementing() {
    create_root_with(
        |_| {
            let owner = get_owner().expect("owner");
            let first = peek_next_child_id(owner).unwrap();
            let second = peek_next_child_id(owner).unwrap();
            let actual = get_next_child_id(owner).unwrap();
            assert_eq!(&*first, "0");
            assert_eq!(&*second, "0");
            assert_eq!(&*actual, "0");
        },
        Some("".into()),
        false,
    );
}

/// `should reflect counter changes from getNextChildId`
#[test]
fn peek_reflects_counter_changes_from_get_next_child_id() {
    create_root_with(
        |_| {
            let owner = get_owner().expect("owner");
            assert_eq!(&*peek_next_child_id(owner).unwrap(), "0");
            get_next_child_id(owner).unwrap();
            assert_eq!(&*peek_next_child_id(owner).unwrap(), "1");
            get_next_child_id(owner).unwrap();
            assert_eq!(&*peek_next_child_id(owner).unwrap(), "2");
        },
        Some("".into()),
        false,
    );
}

/// `should delegate through transparent owners`
#[test]
fn peek_delegates_through_transparent_owners() {
    create_root_with(
        |_| {
            let parent = get_owner().expect("owner");
            create_root_with(
                move |_| {
                    let transparent = get_owner().expect("owner");
                    assert_eq!(&*peek_next_child_id(transparent).unwrap(), "0");
                    get_next_child_id(parent).unwrap();
                    assert_eq!(&*peek_next_child_id(transparent).unwrap(), "1");
                },
                None,
                true,
            );
        },
        Some("".into()),
        false,
    );
}

/// `should throw for owner without id`
#[test]
fn peek_errors_for_owner_without_id() {
    create_root(|_| {
        let owner = get_owner().expect("owner");
        assert_eq!(peek_next_child_id(owner), Err(IdError));
    });
}
