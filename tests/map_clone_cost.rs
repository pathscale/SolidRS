//! Not a ported test. Pins the cost model that `M: Clone` buys.
//!
//! `map_array` returns `Rc<Vec<M>>` compared by `Rc::ptr_eq`, because upstream
//! returns the *same JS array* when a pass produced no structural change and
//! that identity is what stops downstream memos re-running. Rebuilding the
//! vector on a structural pass forces `M: Clone`.
//!
//! The bound is only worth paying if the *unchanged* path costs nothing. If a
//! no-op pass cloned retained rows, `M: Clone` would be buying nothing and the
//! cost would be per row per pass. These tests hold that line.

use solid_rs::*;
use std::cell::Cell;
use std::rc::Rc;

thread_local! {
    static ITEM_CLONES: Cell<usize> = const { Cell::new(0) };
    static M_CLONES: Cell<usize> = const { Cell::new(0) };
}

fn reset() {
    ITEM_CLONES.with(|c| c.set(0));
    M_CLONES.with(|c| c.set(0));
}
fn item_clones() -> usize {
    ITEM_CLONES.with(|c| c.get())
}
fn m_clones() -> usize {
    M_CLONES.with(|c| c.get())
}

#[derive(PartialEq, Eq, Hash, Debug)]
struct Item(String);
impl Clone for Item {
    fn clone(&self) -> Self {
        ITEM_CLONES.with(|c| c.set(c.get() + 1));
        Item(self.0.clone())
    }
}

#[derive(Debug)]
struct Mapped(String);
impl Clone for Mapped {
    fn clone(&self) -> Self {
        M_CLONES.with(|c| c.set(c.get() + 1));
        Mapped(self.0.clone())
    }
}

fn three() -> Vec<Item> {
    vec![Item("a".into()), Item("b".into()), Item("c".into())]
}

/// A pass that produces no structural change must not clone a single mapped
/// value, and must hand back the same `Rc`.
#[test]
fn an_unchanged_pass_clones_no_mapped_values() {
    // `never_equal` so an equal-valued write still notifies. With the default
    // comparator the signal short-circuits and the map pass never runs, which
    // would make this test pass for the wrong reason.
    let (src, set_src) = create_signal_with(three(), Options::never_equal());
    let view = create_root(move |_| {
        map_array(move || src.get(), |it: &Item, _i| Mapped(it.0.clone()))
    });
    let before = view.get();

    reset();
    set_src.set(three());
    flush();
    let after = view.get();

    assert_eq!(m_clones(), 0, "the unchanged path cloned a mapped value");
    assert!(Rc::ptr_eq(&before, &after), "identity must be preserved");

    // The only item clones are the accessor's own: reading a `Vec<Item>` out of
    // a signal yields it by value. `map_array` itself moves rather than clones.
    assert_eq!(item_clones(), 3, "one clone per item, from `src.get()` alone");
}

/// A structural pass clones exactly the retained rows — the inherent price of
/// rebuilding the vector, where upstream copies references. Stated as a number
/// so a regression that starts cloning more is visible.
#[test]
fn a_structural_pass_clones_exactly_the_retained_rows() {
    let (src, set_src) = create_signal_with(three(), Options::never_equal());
    let view = create_root(move |_| {
        map_array(move || src.get(), |it: &Item, _i| Mapped(it.0.clone()))
    });
    let _ = view.get();

    reset();
    set_src.set(vec![
        Item("a".into()),
        Item("b".into()),
        Item("c".into()),
        Item("d".into()),
    ]);
    flush();
    assert_eq!(view.get().len(), 4);
    assert_eq!(
        m_clones(),
        3,
        "the three retained rows are cloned; the new row is mapped, not cloned"
    );
}

/// Removal is the same story from the other side: survivors are cloned once.
#[test]
fn a_removal_clones_only_the_survivors() {
    let (src, set_src) = create_signal_with(three(), Options::never_equal());
    let view = create_root(move |_| {
        map_array(move || src.get(), |it: &Item, _i| Mapped(it.0.clone()))
    });
    let _ = view.get();

    reset();
    set_src.set(vec![Item("a".into()), Item("c".into())]);
    flush();
    assert_eq!(view.get().len(), 2);
    assert_eq!(m_clones(), 2, "two survivors, two clones, nothing remapped");
}
