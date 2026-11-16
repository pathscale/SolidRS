//! Port of `tests/mapArray.test.ts`.
//!
//! # Translating `===` on mapped values
//!
//! Upstream asserts `a === b2` to mean "this row was *not* re-mapped — the same
//! mapped instance moved". Rust values are cloned into the mapped array
//! (`M: Clone`), so pointer identity is not the right instrument. Each mapped
//! row instead carries a `uid` stamped from a counter *at map time*, and the
//! ported assertions compare uids. That tests exactly what upstream's `===`
//! tests: whether the mapper ran again for that row.

mod common;

use common::{Counter, Log};
use solid_rs::*;
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Item {
    id: String,
}

fn item(id: &str) -> Item {
    Item { id: id.to_string() }
}

/// A mapped row for the keyed-by-identity overload: raw item, index accessor.
#[derive(Clone)]
struct Row {
    uid: usize,
    id: String,
    index: ReadSignal<usize>,
}

impl Row {
    fn index(&self) -> usize {
        self.index.get()
    }
}

thread_local! {
    static UID: Cell<usize> = const { Cell::new(0) };
}

fn next_uid() -> usize {
    UID.with(|u| {
        let v = u.get() + 1;
        u.set(v);
        v
    })
}

/// `should compute keyed map`
#[test]
fn should_compute_keyed_map() {
    let (source, set_source) = create_signal(vec![item("a"), item("b"), item("c")]);
    let computed = Counter::new();
    let c = computed.clone();

    let map = create_root(move |_| {
        map_array(
            move || source.get(),
            move |value: &Item, index| {
                c.inc();
                Row {
                    uid: next_uid(),
                    id: value.id.clone(),
                    index,
                }
            },
        )
    });

    let first = map.get();
    let (a, b, c0) = (first[0].clone(), first[1].clone(), first[2].clone());
    assert_eq!(a.id, "a");
    assert_eq!(a.index(), 0);
    assert_eq!(b.id, "b");
    assert_eq!(b.index(), 1);
    assert_eq!(c0.id, "c");
    assert_eq!(c0.index(), 2);
    assert_eq!(computed.get(), 3);

    // Move values around.
    set_source.update(|p| {
        let mut next = p.clone();
        next.swap(0, 1);
        next
    });
    flush();

    let second = map.get();
    let (a2, b2, c2) = (second[0].clone(), second[1].clone(), second[2].clone());
    assert_eq!(a2.id, "b");
    assert_eq!(a2.index(), 0);
    assert_eq!(a.uid, b2.uid, "the 'a' row moved rather than being remapped");
    assert_eq!(b2.id, "a");
    assert_eq!(b2.index(), 1);
    assert_eq!(b.uid, a2.uid);
    assert_eq!(c2.id, "c");
    assert_eq!(c2.index(), 2);
    assert_eq!(c0.uid, c2.uid);
    assert_eq!(computed.get(), 3, "a move maps nothing");

    // Add new value.
    set_source.update(|p| {
        let mut next = p.clone();
        next.push(item("d"));
        next
    });
    flush();

    assert_eq!(map.get().len(), 4);
    assert_eq!(map.get().last().unwrap().id, "d");
    assert_eq!(map.get().last().unwrap().index(), 3);
    assert_eq!(computed.get(), 4);

    // Remove value.
    set_source.update(|p| p[1..].to_vec());
    flush();

    assert_eq!(map.get().len(), 3);
    assert_eq!(map.get()[0].id, "a");
    assert_eq!(map.get()[0].uid, b2.uid);
    assert_eq!(map.get()[0].uid, a.uid);
    assert_eq!(computed.get(), 4);

    // Empty.
    set_source.set(vec![]);
    flush();

    assert_eq!(map.get().len(), 0);
    assert_eq!(computed.get(), 4);
}

/// `should notify observer`
#[test]
fn should_notify_observer() {
    let (source, set_source) = create_signal(vec![item("a"), item("b"), item("c")]);

    let map = create_root(move |_| {
        map_array(move || source.get(), move |value: &Item, _index| value.id.clone())
    });

    let effect = Counter::new();
    let e = effect.clone();
    create_root(move |_| {
        create_effect(move |_| map.get(), move |_v: &Rc<Vec<String>>, _| {
            e.inc();
        });
    });
    flush();

    set_source.update(|p| p[1..].to_vec());
    flush();
    assert_eq!(effect.get(), 2);
}

/// A mapped row for the `keyed: false` overload: value accessor, plain index.
#[derive(Clone)]
struct IdxRow {
    uid: usize,
    value: ReadSignal<i32>,
    index: usize,
}

/// `should compute map when key by index`
#[test]
fn should_compute_map_when_key_by_index() {
    let (source, set_source) = create_signal(vec![1, 2, 3]);
    let computed = Counter::new();
    let c = computed.clone();

    let map = create_root(move |_| {
        map_array_by_index(
            move || source.get(),
            move |value: ReadSignal<i32>, index: usize| {
                c.inc();
                IdxRow {
                    uid: next_uid(),
                    value,
                    index,
                }
            },
        )
    });

    let first = map.get();
    let (a, b, c0) = (first[0].clone(), first[1].clone(), first[2].clone());
    assert_eq!(a.index, 0);
    assert_eq!(a.value.get() * 2, 2);
    assert_eq!(b.index, 1);
    assert_eq!(b.value.get() * 2, 4);
    assert_eq!(c0.index, 2);
    assert_eq!(c0.value.get() * 2, 6);
    assert_eq!(computed.get(), 3);

    // Move values around: by-index rows are positional, so nothing remaps.
    set_source.set(vec![3, 2, 1]);
    flush();

    let second = map.get();
    let (a2, b2, c2) = (second[0].clone(), second[1].clone(), second[2].clone());
    assert_eq!(a2.index, 0);
    assert_eq!(a2.value.get() * 2, 6);
    assert_eq!(a.uid, a2.uid);
    assert_eq!(b2.index, 1);
    assert_eq!(b2.value.get() * 2, 4);
    assert_eq!(b.uid, b2.uid);
    assert_eq!(c2.index, 2);
    assert_eq!(c2.value.get() * 2, 2);
    assert_eq!(c0.uid, c2.uid);
    assert_eq!(computed.get(), 3);

    // Add new value.
    set_source.set(vec![3, 2, 1, 4]);
    flush();

    assert_eq!(map.get().len(), 4);
    assert_eq!(map.get().last().unwrap().index, 3);
    assert_eq!(map.get().last().unwrap().value.get() * 2, 8);
    assert_eq!(computed.get(), 4);

    // Remove value.
    set_source.set(vec![2, 1, 4]);
    flush();

    assert_eq!(map.get().len(), 3);
    assert_eq!(map.get()[0].value.get() * 2, 4);

    // Empty.
    set_source.set(vec![]);
    flush();

    assert_eq!(map.get().len(), 0);
    assert_eq!(computed.get(), 4);
}

/// `should not retain disposed row owners in the parent's sibling chain across
/// full replacements`
///
/// Regression: keyed-by-identity disposes every old row owner on a full
/// replacement, and the disposed owners used to stay linked into their
/// parent's child chain via `next_sibling`, accumulating per replacement.
#[test]
fn should_not_retain_disposed_row_owners_across_full_replacements() {
    const ROWS: usize = 50;
    const REPLACEMENTS: usize = 10;

    let seq = Counter::new();
    let s = seq.clone();
    let make = move || -> Vec<Item> {
        (0..ROWS).map(|_| item(&format!("{}", s.inc()))).collect()
    };

    let (src, set_src) = create_signal(make());
    let root_owner: Rc<Cell<Option<Owner>>> = Rc::new(Cell::new(None));
    let capture = root_owner.clone();

    let dispose = create_root(move |d| {
        let _ = map_array(move || src.get(), move |it: &Item, _i| it.id.clone());
        capture.set(get_owner());
        d
    });

    let root = root_owner.get().expect("root owner");
    let initial = debug::owner_subtree_size(root.node_id());

    for _ in 0..REPLACEMENTS {
        set_src.set(make());
        flush();
    }

    // Pre-fix the total grew by REPLACEMENTS * ROWS; post-fix it is stable.
    assert_eq!(debug::owner_subtree_size(root.node_id()), initial);

    dispose.dispose();
}

/// A mapped row for the `keyed: fn` overload: both accessors.
#[derive(Clone)]
struct KeyedRow {
    uid: usize,
    value: ReadSignal<Item>,
    index: ReadSignal<usize>,
}

/// `should compute custom keyed map`
#[test]
fn should_compute_custom_keyed_map() {
    let (source, set_source) = create_signal(vec![item("a"), item("b"), item("c")]);
    let computed = Counter::new();
    let c = computed.clone();

    let map = create_root(move |_| {
        map_array_keyed(
            move || source.get(),
            |it: &Item| it.id.clone(),
            move |value: ReadSignal<Item>, index: ReadSignal<usize>| {
                c.inc();
                KeyedRow {
                    uid: next_uid(),
                    value,
                    index,
                }
            },
        )
    });

    let first = map.get();
    let (a, b, c0) = (first[0].clone(), first[1].clone(), first[2].clone());
    assert_eq!(a.value.with(|v| v.id.clone()), "a");
    assert_eq!(a.index.get(), 0);
    assert_eq!(b.value.with(|v| v.id.clone()), "b");
    assert_eq!(b.index.get(), 1);
    assert_eq!(c0.value.with(|v| v.id.clone()), "c");
    assert_eq!(c0.index.get(), 2);
    assert_eq!(computed.get(), 3);

    // Move values around.
    set_source.update(|p| {
        let mut next = p.clone();
        next.swap(0, 1);
        next
    });
    flush();

    let second = map.get();
    let (a2, b2, c2) = (second[0].clone(), second[1].clone(), second[2].clone());
    assert_eq!(a2.value.with(|v| v.id.clone()), "b");
    assert_eq!(a.uid, b2.uid);
    assert_eq!(a2.index.get(), 0);
    assert_eq!(b2.value.with(|v| v.id.clone()), "a");
    assert_eq!(b2.index.get(), 1);
    assert_eq!(b.uid, a2.uid);
    assert_eq!(c2.value.with(|v| v.id.clone()), "c");
    assert_eq!(c2.index.get(), 2);
    assert_eq!(c0.uid, c2.uid);
    assert_eq!(computed.get(), 3);

    // Add new value.
    set_source.update(|p| {
        let mut next = p.clone();
        next.push(item("d"));
        next
    });
    flush();

    assert_eq!(map.get().len(), 4);
    assert_eq!(map.get().last().unwrap().value.with(|v| v.id.clone()), "d");
    assert_eq!(map.get().last().unwrap().index.get(), 3);
    assert_eq!(computed.get(), 4);

    // Remove value.
    set_source.update(|p| p[1..].to_vec());
    flush();

    assert_eq!(map.get().len(), 3);
    assert_eq!(map.get()[0].value.with(|v| v.id.clone()), "a");
    assert_eq!(map.get()[0].uid, b2.uid);
    assert_eq!(map.get()[0].uid, a.uid);
    assert_eq!(computed.get(), 4);

    // Empty.
    set_source.set(vec![]);
    flush();

    assert_eq!(map.get().len(), 0);
    assert_eq!(computed.get(), 4);
}

/// `should pass a raw item for keyed-by-reference rows`
#[test]
fn should_pass_a_raw_item_for_keyed_by_reference_rows() {
    let the_item = item("a");
    let (source, _set) = create_signal(vec![the_item.clone()]);
    let received: Rc<Cell<Option<Item>>> = Rc::new(Cell::new(None));
    let sink = received.clone();

    create_root(move |_| {
        map_array(
            move || source.get(),
            move |value: &Item, _i| {
                sink.set(Some(value.clone()));
                value.id.clone()
            },
        )
    });

    assert_eq!(received.take(), Some(the_item));
}

/// `should pass a raw index for keyed:false rows`
#[test]
fn should_pass_a_raw_index_for_keyed_false_rows() {
    let (source, set_source) = create_signal(vec!["a".to_string(), "b".to_string()]);

    #[derive(Clone)]
    struct R {
        uid: usize,
        value: ReadSignal<String>,
        index: usize,
    }

    let map = create_root(move |_| {
        map_array_by_index(
            move || source.get(),
            move |value: ReadSignal<String>, index: usize| R {
                uid: next_uid(),
                value,
                index,
            },
        )
    });

    let first = map.get()[0].clone();
    assert_eq!(first.value.get(), "a");
    assert_eq!(first.index, 0);

    set_source.set(vec!["b".to_string(), "a".to_string()]);
    flush();

    assert_eq!(map.get()[0].uid, first.uid, "same row instance");
    assert_eq!(first.value.get(), "b");
    assert_eq!(first.index, 0);
}

// ---------------------------------------------------------------------------
// The coverage the brief asks for beyond the upstream file. Labelled as
// additions, not as ported tests.
// ---------------------------------------------------------------------------

/// Not upstream. Insert at head, insert at tail, and remove from the middle,
/// each asserting that untouched rows are not remapped.
#[test]
fn structural_edits_do_not_remap_untouched_rows() {
    let (src, set_src) = create_signal(vec![item("a"), item("b"), item("c")]);
    let maps = Counter::new();
    let m = maps.clone();

    let view = create_root(move |_| {
        map_array(
            move || src.get(),
            move |it: &Item, _i| {
                m.inc();
                Row {
                    uid: next_uid(),
                    id: it.id.clone(),
                    index: _i,
                }
            },
        )
    });
    assert_eq!(maps.get(), 3);
    let uid_of = |v: &Rc<Vec<Row>>, id: &str| v.iter().find(|r| r.id == id).unwrap().uid;
    let before = view.get();
    let (ua, ub, uc) = (
        uid_of(&before, "a"),
        uid_of(&before, "b"),
        uid_of(&before, "c"),
    );

    // insert at head
    set_src.update(|p| {
        let mut n = vec![item("z")];
        n.extend(p.clone());
        n
    });
    flush();
    let v = view.get();
    assert_eq!(v.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["z", "a", "b", "c"]);
    assert_eq!(maps.get(), 4, "only the new head mapped");
    assert_eq!((uid_of(&v, "a"), uid_of(&v, "b"), uid_of(&v, "c")), (ua, ub, uc));
    assert_eq!(v[1].index(), 1, "index signals follow the shift");

    // insert at tail
    set_src.update(|p| {
        let mut n = p.clone();
        n.push(item("y"));
        n
    });
    flush();
    let v = view.get();
    assert_eq!(v.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["z", "a", "b", "c", "y"]);
    assert_eq!(maps.get(), 5, "only the new tail mapped");

    // remove from the middle
    set_src.update(|p| p.iter().filter(|i| i.id != "b").cloned().collect());
    flush();
    let v = view.get();
    assert_eq!(v.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["z", "a", "c", "y"]);
    assert_eq!(maps.get(), 5, "a removal maps nothing");
    assert_eq!((uid_of(&v, "a"), uid_of(&v, "c")), (ua, uc));
    assert_eq!(v[2].index(), 2, "the survivor's index signal was updated");
}

/// Not upstream. A full replacement maps every row exactly once and disposes
/// every previous row's scope.
#[test]
fn a_full_replacement_remaps_everything_and_disposes_the_old_scopes() {
    let (src, set_src) = create_signal(vec![item("a"), item("b")]);
    let maps = Counter::new();
    let cleanups = Log::<String>::new();
    let (m, c) = (maps.clone(), cleanups.clone());

    let view = create_root(move |_| {
        map_array(
            move || src.get(),
            move |it: &Item, _i| {
                m.inc();
                let id = it.id.clone();
                let c = c.clone();
                on_cleanup(move || c.push(id));
                it.id.clone()
            },
        )
    });
    assert_eq!(*view.get(), vec!["a".to_string(), "b".to_string()]);
    assert_eq!(maps.get(), 2);

    set_src.set(vec![item("x"), item("y")]);
    flush();

    assert_eq!(*view.get(), vec!["x".to_string(), "y".to_string()]);
    assert_eq!(maps.get(), 4, "both new rows mapped");
    let mut disposed = cleanups.get();
    disposed.sort();
    assert_eq!(disposed, vec!["a".to_string(), "b".to_string()]);
}

/// Not upstream. Disposal ordering for removed rows: a removed row's cleanups
/// run when it leaves, and only that row's do.
#[test]
fn removed_rows_dispose_in_owner_tree_order() {
    let (src, set_src) = create_signal(vec![item("a"), item("b"), item("c")]);
    let events = Log::<String>::new();
    let e = events.clone();

    let view = create_root(move |_| {
        map_array(
            move || src.get(),
            move |it: &Item, _i| {
                let id = it.id.clone();
                let e = e.clone();
                // A nested effect, so the row owns a subtree rather than a
                // single cleanup — disposal has to unwind it in order.
                let inner_id = id.clone();
                let inner_e = e.clone();
                create_effect(
                    move |_| {
                        let e = inner_e.clone();
                        let id = inner_id.clone();
                        on_cleanup(move || e.push(format!("{id}:effect")));
                    },
                    move |_v: &(), _| {},
                );
                on_cleanup(move || e.push(format!("{id}:row")));
                it.id.clone()
            },
        )
    });
    flush();
    assert_eq!(*view.get(), vec!["a".into(), "b".into(), "c".into()] as Vec<String>);
    assert!(events.get().is_empty());

    // Remove the middle row only.
    set_src.update(|p| p.iter().filter(|i| i.id != "b").cloned().collect());
    flush();

    // Children unwind before the owner's own disposal list.
    assert_eq!(events.get(), vec!["b:effect".to_string(), "b:row".to_string()]);

    // And the survivors are untouched.
    set_src.set(vec![]);
    flush();
    let mut rest = events.get()[2..].to_vec();
    rest.sort();
    assert_eq!(
        rest,
        vec![
            "a:effect".to_string(),
            "a:row".to_string(),
            "c:effect".to_string(),
            "c:row".to_string()
        ]
    );
}

/// Not upstream. The mapped array keeps its identity when a pass produces no
/// structural change, which is what stops downstream consumers re-running.
#[test]
fn an_unchanged_pass_keeps_the_mapped_array_identity() {
    let (src, set_src) = create_signal(vec![item("a"), item("b")]);
    let downstream = Counter::new();
    let d = downstream.clone();

    let view = create_root(move |_| {
        let view = map_array(move || src.get(), move |it: &Item, _i| it.id.clone());
        create_render_effect(
            move |_| view.get(),
            move |_v: &Rc<Vec<String>>, _| {
                d.inc();
            },
        );
        view
    });
    flush();
    assert_eq!(downstream.get(), 1);
    let first = view.get();

    // Same items, fresh Vec: every position matches, so the pass is a no-op.
    set_src.set(vec![item("a"), item("b")]);
    flush();

    assert!(Rc::ptr_eq(&first, &view.get()), "same mapped array identity");
    assert_eq!(downstream.get(), 1, "downstream did not re-run");
}

/// Not upstream. `fallback` shows when the list is empty and is disposed when
/// rows return.
#[test]
fn fallback_shows_when_empty_and_is_disposed_when_rows_return() {
    let (src, set_src) = create_signal(vec![item("a")]);
    let fallback_cleanups = Counter::new();
    let fc = fallback_cleanups.clone();

    let view = create_root(move |_| {
        map_array_with(
            move || src.get(),
            move |it: &Item, _i| it.id.clone(),
            MapOptions::default().fallback(move || {
                let fc = fc.clone();
                on_cleanup(move || {
                    fc.inc();
                });
                "empty".to_string()
            }),
        )
    });
    assert_eq!(*view.get(), vec!["a".to_string()]);

    set_src.set(vec![]);
    flush();
    assert_eq!(*view.get(), vec!["empty".to_string()]);
    assert_eq!(fallback_cleanups.get(), 0);

    set_src.set(vec![item("b")]);
    flush();
    assert_eq!(*view.get(), vec!["b".to_string()]);
    assert_eq!(fallback_cleanups.get(), 1, "the fallback scope was disposed");
}
