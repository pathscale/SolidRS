//! Port of `tests/repeat.test.ts`.
//!
//! Three upstream tests source their data from `createStore`. The store is a
//! separate port, so those use a signal holding a `Vec` instead. The
//! substitution is inert for what the tests pin — `repeat` windows on a
//! *count*, and the row callbacks only read the backing data to assert — and it
//! is recorded in `porting-reply.md` §H6.
//!
//! Mapped-value identity is compared through a `uid` stamped at map time, for
//! the reason given at the top of `tests/map_array.rs`.

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

/// A mapped row: the plain index it was created for, plus a handle on the
/// backing list so `id` stays a live read as upstream's getter is.
#[derive(Clone)]
struct Row {
    uid: usize,
    index: usize,
    source: ReadSignal<Vec<Item>>,
}

impl Row {
    fn id(&self) -> String {
        self.source.with(|s| s[self.index].id.clone())
    }
}

/// `should compute keyed map`
#[test]
fn should_compute_keyed_map() {
    let (source, set_source) = create_signal(vec![item("a"), item("b"), item("c")]);
    let computed = Counter::new();
    let c = computed.clone();

    let map = create_root(move |_| {
        repeat(
            move || source.with(|s| s.len()),
            move |index| {
                c.inc();
                Row {
                    uid: next_uid(),
                    index,
                    source,
                }
            },
        )
    });

    let first = map.get();
    let (a, b, c0) = (first[0].clone(), first[1].clone(), first[2].clone());
    assert_eq!(a.id(), "a");
    assert_eq!(a.index, 0);
    assert_eq!(b.id(), "b");
    assert_eq!(b.index, 1);
    assert_eq!(c0.id(), "c");
    assert_eq!(c0.index, 2);
    assert_eq!(computed.get(), 3);

    // Move values around: the count is unchanged, so no row is remapped.
    set_source.update(|p| {
        let mut n = p.clone();
        n.swap(0, 1);
        n
    });
    flush();

    let second = map.get();
    let (a2, b2, c2) = (second[0].clone(), second[1].clone(), second[2].clone());
    assert_eq!(a2.id(), "b");
    assert_eq!(a.uid, a2.uid);
    assert_eq!(a2.index, 0);
    assert_eq!(b2.id(), "a");
    assert_eq!(b2.index, 1);
    assert_eq!(b.uid, b2.uid);
    assert_eq!(c2.id(), "c");
    assert_eq!(c2.index, 2);
    assert_eq!(c0.uid, c2.uid);
    assert_eq!(computed.get(), 3);

    // Add new value.
    set_source.update(|p| {
        let mut n = p.clone();
        n.push(item("d"));
        n
    });
    flush();

    assert_eq!(map.get().len(), 4);
    assert_eq!(map.get().last().unwrap().id(), "d");
    assert_eq!(map.get().last().unwrap().index, 3);
    assert_eq!(computed.get(), 4);

    // Remove value.
    set_source.update(|p| {
        let mut n = p.clone();
        n.pop();
        n
    });
    flush();

    assert_eq!(map.get().len(), 3);
    assert_eq!(map.get()[0].id(), "b");
    assert_eq!(map.get()[0].uid, a2.uid);
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
        repeat(move || source.with(|s| s.len()), move |index| index)
    });

    let effect = Counter::new();
    let e = effect.clone();
    create_root(move |_| {
        create_effect(move |_| map.get(), move |_v: &Rc<Vec<usize>>, _| {
            e.inc();
        });
    });
    flush();

    set_source.update(|p| {
        let mut n = p.clone();
        n.pop();
        n
    });
    flush();
    assert_eq!(effect.get(), 2);
}

/// `should compute map when key by index`
#[test]
fn should_compute_map_when_key_by_index() {
    let (source, set_source) = create_signal(vec![1, 2, 3]);
    let computed = Counter::new();
    let c = computed.clone();

    #[derive(Clone)]
    struct R {
        uid: usize,
        index: usize,
        source: ReadSignal<Vec<i32>>,
    }
    impl R {
        fn id(&self) -> i32 {
            self.source.with(|s| s[self.index]) * 2
        }
    }

    let map = create_root(move |_| {
        repeat(
            move || source.with(|s| s.len()),
            move |index| {
                c.inc();
                R {
                    uid: next_uid(),
                    index,
                    source,
                }
            },
        )
    });

    let first = map.get();
    let (a, b, c0) = (first[0].clone(), first[1].clone(), first[2].clone());
    assert_eq!(a.index, 0);
    assert_eq!(a.id(), 2);
    assert_eq!(b.index, 1);
    assert_eq!(b.id(), 4);
    assert_eq!(c0.index, 2);
    assert_eq!(c0.id(), 6);
    assert_eq!(computed.get(), 3);

    set_source.set(vec![3, 2, 1]);
    flush();

    let second = map.get();
    let (a2, b2, c2) = (second[0].clone(), second[1].clone(), second[2].clone());
    assert_eq!(a2.index, 0);
    assert_eq!(a2.id(), 6);
    assert_eq!(a.uid, a2.uid);
    assert_eq!(b2.index, 1);
    assert_eq!(b2.id(), 4);
    assert_eq!(b.uid, b2.uid);
    assert_eq!(c2.index, 2);
    assert_eq!(c2.id(), 2);
    assert_eq!(c0.uid, c2.uid);
    assert_eq!(computed.get(), 3);

    set_source.set(vec![3, 2, 1, 4]);
    flush();
    assert_eq!(map.get().len(), 4);
    assert_eq!(map.get().last().unwrap().index, 3);
    assert_eq!(map.get().last().unwrap().id(), 8);
    assert_eq!(computed.get(), 4);

    set_source.set(vec![2, 1, 4]);
    flush();
    assert_eq!(map.get().len(), 3);
    assert_eq!(map.get()[0].id(), 4);

    set_source.set(vec![]);
    flush();
    assert_eq!(map.get().len(), 0);
    assert_eq!(computed.get(), 4);
}

/// `disposes the rows that leave the window when `from` advances from a
/// non-zero offset`
#[test]
fn disposes_rows_that_leave_the_window_when_from_advances_from_a_nonzero_offset() {
    let cleanups = Log::<usize>::new();
    let (from, set_from) = create_signal(1usize);

    let c = cleanups.clone();
    let map = create_root(move |_| {
        repeat_with(
            || 3,
            move |index| {
                let c = c.clone();
                on_cleanup(move || c.push(index));
                index
            },
            RepeatOptions::default().from(move || from.get()),
        )
    });

    assert_eq!(*map.get(), vec![1, 2, 3]);

    // Slide the window 1-3 -> 3-5: rows 1 and 2 leave, row 3 stays.
    set_from.set(3);
    flush();

    assert_eq!(*map.get(), vec![3, 4, 5]);
    let mut got = cleanups.get();
    got.sort();
    assert_eq!(got, vec![1, 2]);
}

/// `should retain instances when only `offset` changes`
#[test]
fn should_retain_instances_when_only_offset_changes() {
    let (source, _set) = create_signal(vec![
        item("a"),
        item("b"),
        item("c"),
        item("d"),
        item("e"),
    ]);
    let (count, set_count) = create_signal(3usize);
    let (from, set_from) = create_signal(0usize);
    let computed = Counter::new();
    let c = computed.clone();

    let map = create_root(move |_| {
        repeat_with(
            move || count.get(),
            move |index| {
                c.inc();
                Row {
                    uid: next_uid(),
                    index,
                    source,
                }
            },
            RepeatOptions::default().from(move || from.get()),
        )
    });

    let v = map.get();
    assert_eq!(v.len(), 3);
    assert_eq!(v[0].id(), "a");
    assert_eq!(v[0].index, 0);
    assert_eq!(v[1].id(), "b");
    assert_eq!(v[1].index, 1);
    assert_eq!(v[2].id(), "c");
    assert_eq!(v[2].index, 2);
    assert_eq!(computed.get(), 3);

    set_from.set(2);
    flush();
    let v = map.get();
    assert_eq!(v[0].id(), "c");
    assert_eq!(v[0].index, 2);
    assert_eq!(v[1].id(), "d");
    assert_eq!(v[1].index, 3);
    assert_eq!(v[2].id(), "e");
    assert_eq!(v[2].index, 4);
    assert_eq!(computed.get(), 5, "only rows 3 and 4 were created");

    set_from.set(1);
    flush();
    let v = map.get();
    assert_eq!(v.len(), 3);
    assert_eq!(v[0].id(), "b");
    assert_eq!(v[0].index, 1);
    assert_eq!(v[1].id(), "c");
    assert_eq!(v[1].index, 2);
    assert_eq!(v[2].id(), "d");
    assert_eq!(v[2].index, 3);
    assert_eq!(computed.get(), 6);

    set_count.set(4);
    flush();
    let v = map.get();
    assert_eq!(v.len(), 4);
    assert_eq!(v[0].id(), "b");
    assert_eq!(v[1].id(), "c");
    assert_eq!(v[2].id(), "d");
    assert_eq!(v[3].id(), "e");
    assert_eq!(v[3].index, 4);
    assert_eq!(computed.get(), 7);
}

/// `recovers when count goes to 0, from changes, and count comes back nonzero
/// (#2767)`
#[test]
fn recovers_when_count_goes_to_zero_then_from_changes_and_count_returns() {
    let (count, set_count) = create_signal(2usize);
    let (from, set_from) = create_signal(2usize);

    let map = create_root(move |_| {
        repeat_with(
            move || count.get(),
            |index| index,
            RepeatOptions::default().from(move || from.get()),
        )
    });

    let snapshot = Log::<Vec<usize>>::new();
    let s = snapshot.clone();
    create_root(move |_| {
        create_effect(
            move |_| map.get(),
            move |rows: &Rc<Vec<usize>>, _| s.push((**rows).clone()),
        );
    });
    flush();
    assert_eq!(snapshot.get().last().unwrap(), &vec![2, 3]);

    // 1. clear the rows
    set_count.set(0);
    flush();
    assert_eq!(snapshot.get().last().unwrap(), &Vec::<usize>::new());

    // 2. show row 0 — moves `from` to 0 and `count` to 1 simultaneously.
    // Without the `_offset` reset this indexes a node array that no longer has
    // the entry.
    set_from.set(0);
    set_count.set(1);
    flush();
    assert_eq!(snapshot.get().last().unwrap(), &vec![0]);
}

/// `slides the window backwards: disposes departing rows and retains the
/// overlap (#2767)`
#[test]
fn slides_the_window_backwards_disposing_departing_rows_and_retaining_the_overlap() {
    let (from, set_from) = create_signal(3usize);
    let disposed = Log::<usize>::new();
    let d = disposed.clone();

    #[derive(Clone)]
    struct R {
        uid: usize,
        index: usize,
    }

    let map = create_root(move |_| {
        repeat_with(
            || 3,
            move |index| {
                let d = d.clone();
                on_cleanup(move || d.push(index));
                R {
                    uid: next_uid(),
                    index,
                }
            },
            RepeatOptions::default().from(move || from.get()),
        )
    });

    let snapshot = Log::<Vec<R>>::new();
    let s = snapshot.clone();
    create_root(move |_| {
        create_effect(
            move |_| map.get(),
            move |rows: &Rc<Vec<R>>, _| s.push((**rows).clone()),
        );
    });
    flush();
    let first = snapshot.get().last().unwrap().clone();
    assert_eq!(first.iter().map(|r| r.index).collect::<Vec<_>>(), vec![3, 4, 5]);
    let retained = first[0].uid; // the row at global index 3

    // window 3-5 -> 1-3: rows 4 and 5 leave, rows 1 and 2 enter, row 3 stays
    set_from.set(1);
    flush();
    let second = snapshot.get().last().unwrap().clone();
    assert_eq!(second.iter().map(|r| r.index).collect::<Vec<_>>(), vec![1, 2, 3]);
    // the overlap row is the same instance, now at the tail
    assert_eq!(second[2].uid, retained);
    // only the departing rows were disposed
    let mut got = disposed.get();
    got.sort();
    assert_eq!(got, vec![4, 5]);
}

// ---------------------------------------------------------------------------
// `describe("disjoint window jumps")`
// ---------------------------------------------------------------------------

struct Windowed {
    view: Memo<Rc<Vec<usize>>>,
    set_from: WriteSignal<usize>,
    created: Counter,
    cleaned: Counter,
}

impl Windowed {
    fn jump(&self, n: usize) {
        self.set_from.set(n);
        flush();
    }
    fn live(&self) -> usize {
        self.created.get() - self.cleaned.get()
    }
}

fn windowed() -> Windowed {
    let (from, set_from) = create_signal(0usize);
    let created = Counter::new();
    let cleaned = Counter::new();
    let (cr, cl) = (created.clone(), cleaned.clone());

    let view = create_root(move |_| {
        repeat_with(
            || 3,
            move |i| {
                cr.inc();
                let cl = cl.clone();
                on_cleanup(move || {
                    cl.inc();
                });
                i
            },
            RepeatOptions::default().from(move || from.get()),
        )
    });
    flush();
    Windowed {
        view,
        set_from,
        created,
        cleaned,
    }
}

/// `a forward jump larger than the window creates only the window's rows`
#[test]
fn a_forward_jump_larger_than_the_window_creates_only_the_windows_rows() {
    let w = windowed();
    assert_eq!(*w.view.get(), vec![0, 1, 2]);
    assert_eq!(w.live(), 3);
    w.jump(10);
    assert_eq!(*w.view.get(), vec![10, 11, 12]);
    assert_eq!(w.live(), 3);
}

/// `repeated disjoint jumps do not accumulate live scopes`
#[test]
fn repeated_disjoint_jumps_do_not_accumulate_live_scopes() {
    let w = windowed();
    w.jump(10);
    w.jump(20);
    assert_eq!(*w.view.get(), vec![20, 21, 22]);
    assert_eq!(w.live(), 3);
}

/// `a backward disjoint jump maps the new window without crashing`
#[test]
fn a_backward_disjoint_jump_maps_the_new_window_without_crashing() {
    let w = windowed();
    w.jump(20);
    w.jump(0);
    assert_eq!(*w.view.get(), vec![0, 1, 2]);
    assert_eq!(w.live(), 3);
}

/// `initial render with a nonzero `from` maps only the window`
#[test]
fn initial_render_with_a_nonzero_from_maps_only_the_window() {
    let (from, _set) = create_signal(10usize);
    let created = Counter::new();
    let c = created.clone();

    let view = create_root(move |_| {
        repeat_with(
            || 3,
            move |i| {
                c.inc();
                i
            },
            RepeatOptions::default().from(move || from.get()),
        )
    });
    flush();
    assert_eq!(*view.get(), vec![10, 11, 12]);
    assert_eq!(created.get(), 3);
}
