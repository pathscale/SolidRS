//! Port of `src/map.ts`: `map_array` and `repeat`.
//!
//! These are **reactive primitives, not renderers**. `map_array` maintains a
//! mapped array under keyed reconciliation and owns one scope per row, so a
//! removed row's effects and cleanups run at the right point in the owner tree.
//! Nothing here touches a DOM, and the crate's single dependency (`slotmap`)
//! is unchanged.
//!
//! # Shape changes from the TypeScript
//!
//! * **Four overloads become three functions.** Upstream selects behaviour from
//!   `options.keyed` *and* from `map.length` (JS function arity). Rust closures
//!   have no introspectable arity, so the overloads are named:
//!   [`map_array`] (keyed by identity), [`map_array_by_index`]
//!   (`keyed: false`), [`map_array_keyed`] (`keyed: item => key`).
//! * **The mapped array is `Rc<Vec<M>>`.** Upstream returns the *same JS array*
//!   when a pass produced no structural change and a *fresh* one otherwise —
//!   array identity is load-bearing, because it is what stops downstream memos
//!   re-running. `Rc` plus `Rc::ptr_eq` as the memo's comparator reproduces
//!   that exactly.
//! * **`$TRACK` is absent.** Upstream reads it to subscribe to a store proxy's
//!   structure. The store is a separate port; here `list` is an accessor whose
//!   read already tracks.
//!
//! Every judgement call is recorded in PORTING-QUESTIONS.md §H.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::rc::Rc;

use crate::constants::config;
use crate::core::{create_computed, create_signal_node, set_signal, ComputedSpec, SignalSpec};
use crate::node::{AnyValue, ComputeOutcome, EqualsFn, NodeId};
use crate::owner::{create_owner, dispose_children};
use crate::runtime::{node, with_rt};
use crate::signals::{Memo, ReadSignal};
use crate::Owner;

/// What the row callback receives. Each public constructor fills in the fields
/// its overload provides and leaves the rest absent.
struct RowCtx<Item: 'static> {
    /// The raw row value. Upstream's keyed-by-identity overload passes this
    /// directly rather than an accessor.
    raw: Item,
    /// Present for the `keyed: false` and `keyed: fn` overloads.
    row: Option<ReadSignal<Item>>,
    /// Present when the overload provides an index *accessor*.
    index_sig: Option<ReadSignal<usize>>,
    /// The plain index, for the `keyed: false` overload.
    index: usize,
}

type Mapper<Item, M> = Box<dyn FnMut(RowCtx<Item>) -> M>;
type KeyFn<Item, K> = Rc<dyn Fn(&Item) -> K>;
type SamePosition<Item> = Rc<dyn Fn(&Item, &Item) -> bool>;

/// Port of `MapData`.
struct MapData<Item: 'static, M: 'static, K: 'static> {
    owner: NodeId,
    len: usize,
    list: Box<dyn Fn() -> Vec<Item>>,
    items: Vec<Item>,
    mappings: Rc<Vec<M>>,
    nodes: Vec<NodeId>,
    mapper: Mapper<Item, M>,
    key: KeyFn<Item, K>,
    /// Port of `old === new || (this._rows && compare(this._key, old, new))`,
    /// fused into one predicate.
    ///
    /// A key match subsumes reference equality (identical values yield
    /// identical keys), so the fusion is behaviourally identical — and it
    /// avoids demanding `PartialEq` on `Item` in the keyed-by-function mode,
    /// where upstream needs no such capability.
    same_position: SamePosition<Item>,
    has_rows: bool,
    has_indexes: bool,
    /// Per-row value signals; empty unless `has_rows`.
    rows: Vec<NodeId>,
    /// Per-row index signals; empty unless `has_indexes`.
    indexes: Vec<NodeId>,
    /// Comparator for the row signals, built where `Item: PartialEq` is known.
    row_eq: EqualsFn,
    fallback: Option<Box<dyn Fn() -> M>>,
}

type Shared<Item, M, K> = Rc<RefCell<MapData<Item, M, K>>>;

/// Options shared by the `map_array` family. Port of the `options` bag minus
/// `keyed`, which is encoded in the function name.
pub struct MapOptions<M: 'static> {
    fallback: Option<Box<dyn Fn() -> M>>,
}

impl<M: 'static> Default for MapOptions<M> {
    fn default() -> Self {
        MapOptions { fallback: None }
    }
}

impl<M: 'static> std::fmt::Debug for MapOptions<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapOptions")
            .field("fallback", &self.fallback.is_some())
            .finish()
    }
}

impl<M: 'static> MapOptions<M> {
    pub fn new() -> Self {
        Self::default()
    }

    /// A value to show when the input list is empty.
    pub fn fallback(mut self, f: impl Fn() -> M + 'static) -> Self {
        self.fallback = Some(Box::new(f));
        self
    }
}

// --------------------------------------------------------------- public API

/// Reactively map an array, reusing the previously-mapped value for unchanged
/// items. Keyed **by identity** — upstream's default overload.
///
/// The callback receives the raw item and an index *accessor*. An item that
/// moves is mapped once, not remapped: its mapped value and its owner scope
/// travel with it.
///
/// ```
/// use solid_rs::*;
/// let (src, set_src) = create_signal(vec!["a".to_string(), "b".to_string()]);
/// let view = create_root(move |_| {
///     map_array(move || src.get(), |item: &String, _i| item.to_uppercase())
/// });
/// assert_eq!(*view.get(), vec!["A".to_string(), "B".to_string()]);
///
/// set_src.set(vec!["b".to_string(), "a".to_string()]);
/// flush();
/// assert_eq!(*view.get(), vec!["B".to_string(), "A".to_string()]);
/// ```
pub fn map_array<Item, M>(
    list: impl Fn() -> Vec<Item> + 'static,
    map: impl FnMut(&Item, ReadSignal<usize>) -> M + 'static,
) -> Memo<Rc<Vec<M>>>
where
    Item: Clone + Eq + Hash + 'static,
    M: Clone + 'static,
{
    map_array_with(list, map, MapOptions::default())
}

/// [`map_array`] with options.
pub fn map_array_with<Item, M>(
    list: impl Fn() -> Vec<Item> + 'static,
    map: impl FnMut(&Item, ReadSignal<usize>) -> M + 'static,
    options: MapOptions<M>,
) -> Memo<Rc<Vec<M>>>
where
    Item: Clone + Eq + Hash + 'static,
    M: Clone + 'static,
{
    let mut map = map;
    let mapper: Mapper<Item, M> = Box::new(move |ctx: RowCtx<Item>| {
        map(&ctx.raw, ctx.index_sig.expect("identity mode provides an index accessor"))
    });
    build(
        list,
        mapper,
        Rc::new(|i: &Item| i.clone()),
        // Identity mode has no rows, so `same_position` is never consulted;
        // upstream's `a === b` disjunct is the whole test and is applied
        // directly by the caller.
        Rc::new(|a: &Item, b: &Item| a == b),
        false,
        true,
        item_equals::<Item>(),
        options,
    )
}

/// Reactively map an array **by index** — upstream's `keyed: false` overload.
///
/// Rows are positional: the callback receives a value *accessor* and a plain
/// index. A changed value at a position updates that row's signal rather than
/// remapping it, so the mapped value's identity is stable per position.
pub fn map_array_by_index<Item, M>(
    list: impl Fn() -> Vec<Item> + 'static,
    map: impl FnMut(ReadSignal<Item>, usize) -> M + 'static,
) -> Memo<Rc<Vec<M>>>
where
    Item: Clone + Eq + Hash + 'static,
    M: Clone + 'static,
{
    map_array_by_index_with(list, map, MapOptions::default())
}

/// [`map_array_by_index`] with options.
pub fn map_array_by_index_with<Item, M>(
    list: impl Fn() -> Vec<Item> + 'static,
    map: impl FnMut(ReadSignal<Item>, usize) -> M + 'static,
    options: MapOptions<M>,
) -> Memo<Rc<Vec<M>>>
where
    Item: Clone + Eq + Hash + 'static,
    M: Clone + 'static,
{
    let mut map = map;
    let mapper: Mapper<Item, M> = Box::new(move |ctx: RowCtx<Item>| {
        map(ctx.row.expect("by-index mode provides a row accessor"), ctx.index)
    });
    build(
        list,
        mapper,
        Rc::new(|i: &Item| i.clone()),
        // `compare(undefined, a, b)` is `true`: by-index rows match
        // positionally, whatever the values.
        Rc::new(|_a: &Item, _b: &Item| true),
        true,
        false,
        item_equals::<Item>(),
        options,
    )
}

/// Reactively map an array keyed by an extracted key — upstream's
/// `keyed: item => key` overload.
///
/// The callback receives accessors for both the value and the index. Identity
/// follows the key, so a row whose key is unchanged keeps its mapped value and
/// its scope even when the value itself changed.
pub fn map_array_keyed<Item, M, K>(
    list: impl Fn() -> Vec<Item> + 'static,
    key: impl Fn(&Item) -> K + 'static,
    map: impl FnMut(ReadSignal<Item>, ReadSignal<usize>) -> M + 'static,
) -> Memo<Rc<Vec<M>>>
where
    Item: Clone + PartialEq + 'static,
    M: Clone + 'static,
    K: Eq + Hash + 'static,
{
    map_array_keyed_with(list, key, map, MapOptions::default())
}

/// [`map_array_keyed`] with options.
pub fn map_array_keyed_with<Item, M, K>(
    list: impl Fn() -> Vec<Item> + 'static,
    key: impl Fn(&Item) -> K + 'static,
    map: impl FnMut(ReadSignal<Item>, ReadSignal<usize>) -> M + 'static,
    options: MapOptions<M>,
) -> Memo<Rc<Vec<M>>>
where
    Item: Clone + PartialEq + 'static,
    M: Clone + 'static,
    K: Eq + Hash + 'static,
{
    let mut map = map;
    let mapper: Mapper<Item, M> = Box::new(move |ctx: RowCtx<Item>| {
        map(
            ctx.row.expect("keyed mode provides a row accessor"),
            ctx.index_sig.expect("keyed mode provides an index accessor"),
        )
    });
    let key = Rc::new(key);
    let key_for_cmp = key.clone();
    build(
        list,
        mapper,
        Rc::new(move |i: &Item| key(i)),
        Rc::new(move |a: &Item, b: &Item| key_for_cmp(a) == key_for_cmp(b)),
        true,
        true,
        item_equals::<Item>(),
        options,
    )
}

// ------------------------------------------------------------------ internals

fn item_equals<Item: PartialEq + 'static>() -> EqualsFn {
    Rc::new(|a: &dyn std::any::Any, b: &dyn std::any::Any| {
        match (a.downcast_ref::<Item>(), b.downcast_ref::<Item>()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    })
}

fn usize_equals() -> EqualsFn {
    Rc::new(|a: &dyn std::any::Any, b: &dyn std::any::Any| {
        a.downcast_ref::<usize>() == b.downcast_ref::<usize>()
    })
}

/// The memo comparator reproducing JS array identity: an unchanged pass returns
/// the same `Rc`, so downstream consumers do not re-run.
fn identity_equals<M: 'static>() -> EqualsFn {
    Rc::new(|a: &dyn std::any::Any, b: &dyn std::any::Any| {
        match (a.downcast_ref::<Rc<Vec<M>>>(), b.downcast_ref::<Rc<Vec<M>>>()) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn build<Item, M, K>(
    list: impl Fn() -> Vec<Item> + 'static,
    mapper: Mapper<Item, M>,
    key: KeyFn<Item, K>,
    same_position: SamePosition<Item>,
    has_rows: bool,
    has_indexes: bool,
    row_eq: EqualsFn,
    options: MapOptions<M>,
) -> Memo<Rc<Vec<M>>>
where
    Item: Clone + 'static,
    M: Clone + 'static,
    K: Eq + Hash + 'static,
{
    with_rt(|rt| {
        let owner = create_owner(rt, None, false);
        let data: Shared<Item, M, K> = Rc::new(RefCell::new(MapData {
            owner,
            len: 0,
            list: Box::new(list),
            items: Vec::new(),
            mappings: Rc::new(Vec::new()),
            nodes: Vec::new(),
            mapper,
            key,
            same_position,
            has_rows,
            has_indexes,
            rows: Vec::new(),
            indexes: Vec::new(),
            row_eq,
            fallback: options.fallback,
        }));

        let compute_data = data.clone();
        let compute: crate::node::ComputeFn = Box::new(move |_prev| {
            let out = update_keyed_map(&compute_data);
            Ok(ComputeOutcome::Value(Rc::new(out) as AnyValue))
        });

        let id = create_computed(
            rt,
            ComputedSpec {
                compute,
                equals: Some(identity_equals::<M>()),
                unobserved: None,
                id: None,
                name: "mapArray",
                transparent: false,
                owned_write: false,
                lazy: false,
                sync: true,
                loading_value: None,
                effect_kind: None,
            },
        );

        {
            let nodes = rt.nodes();
            // Untracked reads inside the internal owner resolve through
            // `root_parent_computed`, so a row callback's reads register with
            // the map node rather than vanishing.
            node(&nodes, owner).root_parent_computed.set(Some(id));
            let n = node(&nodes, id);
            n.config.set(n.config.get() & !config::AUTO_DISPOSE);
        }
        Memo::from_id(id)
    })
}

/// Create one row: its signals (per mode), then the user's mapper under the
/// row's own owner. Port of the `mapper` closure family in `updateKeyedMap`.
fn make_row<Item, M, K>(
    data: &Shared<Item, M, K>,
    row_owner: NodeId,
    j: usize,
    item: &Item,
) -> (Option<NodeId>, Option<NodeId>, M)
where
    Item: Clone + 'static,
    M: 'static,
    K: 'static,
{
    let (has_rows, has_indexes, row_eq) = {
        let d = data.borrow();
        (d.has_rows, d.has_indexes, d.row_eq.clone())
    };

    let row_id = if has_rows {
        let v = item.clone();
        Some(with_rt(|rt| {
            create_signal_node(
                rt,
                SignalSpec {
                    value: Some(Rc::new(v) as AnyValue),
                    equals: Some(row_eq),
                    unobserved: None,
                    // Port of `pureOptions = { ownedWrite: true }`: the
                    // reconciler writes these from inside an owned scope,
                    // which the dev guard would otherwise reject.
                    owned_write: true,
                    name: "mapRow",
                },
            )
        }))
    } else {
        None
    };

    let index_id = if has_indexes {
        Some(with_rt(|rt| {
            create_signal_node(
                rt,
                SignalSpec {
                    value: Some(Rc::new(j) as AnyValue),
                    equals: Some(usize_equals()),
                    unobserved: None,
                    owned_write: true,
                    name: "mapIndex",
                },
            )
        }))
    } else {
        None
    };

    let ctx = RowCtx {
        raw: item.clone(),
        row: row_id.map(ReadSignal::<Item>::from_id),
        index_sig: index_id.map(ReadSignal::<usize>::from_id),
        index: j,
    };

    let value = crate::run_with_owner(Some(Owner(row_owner)), || {
        let mut d = data.borrow_mut();
        (d.mapper)(ctx)
    });
    (row_id, index_id, value)
}

fn write_row<Item: Clone + 'static>(sig: NodeId, v: Item) {
    with_rt(|rt| set_signal(rt, sig, move |_| Rc::new(v) as AnyValue));
}

fn write_index(sig: NodeId, j: usize) {
    with_rt(|rt| set_signal(rt, sig, move |_| Rc::new(j) as AnyValue));
}

fn dispose_owner(id: NodeId) {
    with_rt(|rt| dispose_children(rt, id, true, false));
}

/// Port of `updateKeyedMap`.
///
/// Follows upstream's staged-commit discipline (#2903): rows are created into
/// temporaries and removals deferred, and `data` is only written once every
/// mapper has run. The ordering consequence is preserved — removed rows
/// dispose *after* the pass's new rows are created. With an infallible mapper
/// there is no abort path to take; see PORTING-QUESTIONS §H4.
fn update_keyed_map<Item, M, K>(data: &Shared<Item, M, K>) -> Rc<Vec<M>>
where
    Item: Clone + 'static,
    M: Clone + 'static,
    K: Eq + Hash + 'static,
{
    let (new_items, owner) = {
        let d = data.borrow();
        ((d.list)(), d.owner)
    };
    let new_len = new_items.len();

    crate::run_with_owner(Some(Owner(owner)), || {
        let (old_len, has_rows, has_indexes) = {
            let d = data.borrow();
            (d.len, d.has_rows, d.has_indexes)
        };

        // ---------------------------------------------------- empty fast path
        if new_len == 0 {
            if old_len != 0 {
                with_rt(|rt| dispose_children(rt, owner, false, false));
                let mut d = data.borrow_mut();
                d.nodes = Vec::new();
                d.items = Vec::new();
                d.mappings = Rc::new(Vec::new());
                d.len = 0;
                d.rows = Vec::new();
                d.indexes = Vec::new();
            }
            let needs_fallback = {
                let d = data.borrow();
                d.fallback.is_some() && d.mappings.is_empty()
            };
            if needs_fallback {
                // An aborted fallback attempt leaves an owner without a
                // mapping; dispose it before re-creating.
                if let Some(o) = data.borrow().nodes.first().copied() {
                    dispose_owner(o);
                }
                let row_owner = with_rt(|rt| create_owner(rt, None, false));
                let value = crate::run_with_owner(Some(Owner(row_owner)), || {
                    let d = data.borrow();
                    (d.fallback.as_ref().expect("checked"))()
                });
                let mut d = data.borrow_mut();
                d.nodes = vec![row_owner];
                d.mappings = Rc::new(vec![value]);
            }
            return;
        }

        // ------------------------------------------------- create fast path
        if old_len == 0 {
            let mut mappings: Vec<M> = Vec::with_capacity(new_len);
            let mut nodes: Vec<NodeId> = Vec::with_capacity(new_len);
            let mut rows: Vec<NodeId> = Vec::new();
            let mut indexes: Vec<NodeId> = Vec::new();

            for (j, item) in new_items.iter().enumerate() {
                let row_owner = with_rt(|rt| create_owner(rt, None, false));
                nodes.push(row_owner);
                let (r, x, value) = make_row(data, row_owner, j, item);
                if let Some(r) = r {
                    rows.push(r);
                }
                if let Some(x) = x {
                    indexes.push(x);
                }
                mappings.push(value);
            }

            // Commit; dispose the previous fallback, if any.
            if let Some(o) = data.borrow().nodes.first().copied() {
                dispose_owner(o);
            }
            let mut d = data.borrow_mut();
            d.mappings = Rc::new(mappings);
            d.nodes = nodes;
            if has_rows {
                d.rows = rows;
            }
            if has_indexes {
                d.indexes = indexes;
            }
            d.items = new_items.clone();
            d.len = new_len;
            return;
        }

        // ------------------------------------------------------ general path

        // Skip the common prefix.
        let mut start = 0usize;
        {
            let d = data.borrow();
            let limit = old_len.min(new_len);
            while start < limit
                && (d.same_position)(&d.items[start], &new_items[start])
            {
                start += 1;
            }
        }
        // The prefix's row signals take the new values. A separate pass so the
        // borrow above is released before `set_signal` runs.
        if has_rows {
            for (i, value) in new_items.iter().take(start).enumerate() {
                let sig = data.borrow().rows[i];
                write_row(sig, value.clone());
            }
        }

        // Skip the common suffix — counted only.
        let (mut end, mut new_end) = (old_len as isize - 1, new_len as isize - 1);
        {
            let d = data.borrow();
            while end >= start as isize
                && new_end >= start as isize
                && (d.same_position)(&d.items[end as usize], &new_items[new_end as usize])
            {
                end -= 1;
                new_end -= 1;
            }
        }

        // No structural change: keep the same mapped-array identity, so
        // downstream consumers do not re-run at all.
        if start == new_len && old_len == new_len {
            data.borrow_mut().items = new_items.clone();
            return;
        }

        let dif = new_len as isize - old_len as isize;
        let mut temp: Vec<Option<M>> = (0..new_len).map(|_| None).collect();
        let mut temp_nodes: Vec<Option<NodeId>> = vec![None; new_len];
        let mut rows: Vec<Option<NodeId>> = vec![None; new_len];
        let mut indexes: Vec<Option<NodeId>> = vec![None; new_len];

        // 0) Map every key in the changed window of `new_items`, scanned
        // backwards so duplicates chain in natural order.
        let mut new_indices: HashMap<K, isize> = HashMap::new();
        let mut new_indices_next: Vec<isize> = vec![-1; new_len];
        {
            let d = data.borrow();
            let mut j = new_end;
            while j >= start as isize {
                let key = (d.key)(&new_items[j as usize]);
                new_indices_next[j as usize] = new_indices.get(&key).copied().unwrap_or(-1);
                new_indices.insert(key, j);
                j -= 1;
            }
        }

        // 1) Walk the old changed window: stage survivors at their new
        // positions, queue the rest for disposal at commit.
        let mut removed: Vec<NodeId> = Vec::new();
        {
            let d = data.borrow();
            let mut i = start as isize;
            while i <= end {
                let iu = i as usize;
                let key = (d.key)(&d.items[iu]);
                match new_indices.get(&key).copied() {
                    Some(j) if j != -1 => {
                        let ju = j as usize;
                        temp[ju] = Some(d.mappings[iu].clone());
                        temp_nodes[ju] = Some(d.nodes[iu]);
                        if has_rows {
                            rows[ju] = Some(d.rows[iu]);
                        }
                        if has_indexes {
                            indexes[ju] = Some(d.indexes[iu]);
                        }
                        let next = new_indices_next[ju];
                        new_indices.insert(key, next);
                    }
                    _ => removed.push(d.nodes[iu]),
                }
                i += 1;
            }
        }

        // 2) Create the rows with no survivor, into the temporaries.
        {
            let mut j = start as isize;
            while j <= new_end {
                let ju = j as usize;
                if temp_nodes[ju].is_none() {
                    let row_owner = with_rt(|rt| create_owner(rt, None, false));
                    temp_nodes[ju] = Some(row_owner);
                    let (r, x, value) = make_row(data, row_owner, ju, &new_items[ju]);
                    rows[ju] = r;
                    indexes[ju] = x;
                    temp[ju] = Some(value);
                }
                j += 1;
            }
        }

        // 3) Commit: land the retained prefix and suffix around the staged
        // window, swap in the fresh arrays, then dispose the exited rows.
        {
            let d = data.borrow();
            for i in 0..start {
                temp[i] = Some(d.mappings[i].clone());
                temp_nodes[i] = Some(d.nodes[i]);
                if has_rows {
                    rows[i] = Some(d.rows[i]);
                }
                if has_indexes {
                    indexes[i] = Some(d.indexes[i]);
                }
            }
            for j in ((new_end + 1).max(0) as usize)..new_len {
                let src = (j as isize - dif) as usize;
                temp[j] = Some(d.mappings[src].clone());
                temp_nodes[j] = Some(d.nodes[src]);
                if has_rows {
                    rows[j] = Some(d.rows[src]);
                }
                if has_indexes {
                    indexes[j] = Some(d.indexes[src]);
                }
            }
        }

        // Signal writes for the staged window, then for the shifted suffix.
        {
            let mut j = start as isize;
            while j <= new_end {
                let ju = j as usize;
                if has_rows {
                    write_row(rows[ju].expect("staged row"), new_items[ju].clone());
                }
                if has_indexes {
                    write_index(indexes[ju].expect("staged index"), ju);
                }
                j += 1;
            }
            for j in ((new_end + 1).max(0) as usize)..new_len {
                if has_rows {
                    write_row(rows[j].expect("suffix row"), new_items[j].clone());
                }
                if has_indexes && dif != 0 {
                    write_index(indexes[j].expect("suffix index"), j);
                }
            }
        }

        {
            let mut d = data.borrow_mut();
            d.mappings = Rc::new(temp.into_iter().map(|m| m.expect("committed")).collect());
            d.nodes = temp_nodes.into_iter().map(|n| n.expect("committed")).collect();
            if has_rows {
                d.rows = rows.into_iter().map(|n| n.expect("committed")).collect();
            }
            if has_indexes {
                d.indexes = indexes.into_iter().map(|n| n.expect("committed")).collect();
            }
            d.len = new_len;
            d.items = new_items.clone();
        }

        for o in removed {
            dispose_owner(o);
        }
    });

    let out = data.borrow().mappings.clone();
    out
}

// ---------------------------------------------------------------------- repeat

/// Port of `RepeatData`.
struct RepeatData<M: 'static> {
    owner: NodeId,
    len: usize,
    offset: usize,
    count: Box<dyn Fn() -> usize>,
    map: Box<dyn FnMut(usize) -> M>,
    mappings: Rc<Vec<M>>,
    nodes: Vec<NodeId>,
    from: Option<Box<dyn Fn() -> usize>>,
    fallback: Option<Box<dyn Fn() -> M>>,
}

/// Options for [`repeat`].
pub struct RepeatOptions<M: 'static> {
    from: Option<Box<dyn Fn() -> usize>>,
    fallback: Option<Box<dyn Fn() -> M>>,
}

impl<M: 'static> Default for RepeatOptions<M> {
    fn default() -> Self {
        RepeatOptions {
            from: None,
            fallback: None,
        }
    }
}

impl<M: 'static> std::fmt::Debug for RepeatOptions<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepeatOptions")
            .field("from", &self.from.is_some())
            .field("fallback", &self.fallback.is_some())
            .finish()
    }
}

impl<M: 'static> RepeatOptions<M> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start index (default `0`) — for offset or windowed mapping.
    pub fn from(mut self, f: impl Fn() -> usize + 'static) -> Self {
        self.from = Some(Box::new(f));
        self
    }

    /// A value to show when the count is `0`.
    pub fn fallback(mut self, f: impl Fn() -> M + 'static) -> Self {
        self.fallback = Some(Box::new(f));
        self
    }
}

/// Reactively map `count` indices, reusing previously-mapped entries when only
/// the count changes. Underlying helper for `<Repeat>`.
///
/// ```
/// use solid_rs::*;
/// let (count, set_count) = create_signal(3usize);
/// let view = create_root(move |_| repeat(move || count.get(), |i| i * 10));
/// assert_eq!(*view.get(), vec![0, 10, 20]);
///
/// set_count.set(1);
/// flush();
/// assert_eq!(*view.get(), vec![0]);
/// ```
pub fn repeat<M>(
    count: impl Fn() -> usize + 'static,
    map: impl FnMut(usize) -> M + 'static,
) -> Memo<Rc<Vec<M>>>
where
    M: Clone + 'static,
{
    repeat_with(count, map, RepeatOptions::default())
}

/// [`repeat`] with options.
pub fn repeat_with<M>(
    count: impl Fn() -> usize + 'static,
    map: impl FnMut(usize) -> M + 'static,
    options: RepeatOptions<M>,
) -> Memo<Rc<Vec<M>>>
where
    M: Clone + 'static,
{
    with_rt(|rt| {
        let owner = create_owner(rt, None, false);
        let data = Rc::new(RefCell::new(RepeatData {
            owner,
            len: 0,
            offset: 0,
            count: Box::new(count),
            map: Box::new(map),
            mappings: Rc::new(Vec::new()),
            nodes: Vec::new(),
            from: options.from,
            fallback: options.fallback,
        }));

        let compute_data = data.clone();
        let compute: crate::node::ComputeFn = Box::new(move |_prev| {
            let out = update_repeat(&compute_data);
            Ok(ComputeOutcome::Value(Rc::new(out) as AnyValue))
        });

        let id = create_computed(
            rt,
            ComputedSpec {
                compute,
                equals: Some(identity_equals::<M>()),
                unobserved: None,
                id: None,
                name: "repeat",
                transparent: false,
                owned_write: false,
                lazy: false,
                sync: true,
                loading_value: None,
                effect_kind: None,
            },
        );

        {
            let nodes = rt.nodes();
            node(&nodes, owner).root_parent_computed.set(Some(id));
            let n = node(&nodes, id);
            n.config.set(n.config.get() & !config::AUTO_DISPOSE);
        }
        Memo::from_id(id)
    })
}

/// Port of `updateRepeat`.
///
/// Same staged-commit discipline as `update_keyed_map`: the retained window
/// overlap is copied into fresh arrays, missing indices are created into them,
/// and `data` is only touched — disposal of rows leaving the window included —
/// after every `map` call succeeded. The overlap arithmetic subsumes upstream's
/// former disjoint-window / front-clear / end-clear / shift special cases.
fn update_repeat<M: Clone + 'static>(data: &Rc<RefCell<RepeatData<M>>>) -> Rc<Vec<M>> {
    let (new_len, from, owner) = {
        let d = data.borrow();
        let new_len = (d.count)();
        let from = d.from.as_ref().map(|f| f()).unwrap_or(0);
        (new_len, from, d.owner)
    };

    crate::run_with_owner(Some(Owner(owner)), || {
        let old_len = data.borrow().len;

        if new_len == 0 {
            if old_len != 0 {
                with_rt(|rt| dispose_children(rt, owner, false, false));
                let mut d = data.borrow_mut();
                d.nodes = Vec::new();
                d.mappings = Rc::new(Vec::new());
                d.len = 0;
                // Reset the offset to match the cleared data (upstream #2767,
                // repro 2): a stale offset makes the next non-empty pass index
                // a node array that no longer has that entry.
                d.offset = 0;
            }
            let needs_fallback = {
                let d = data.borrow();
                d.fallback.is_some() && d.mappings.is_empty()
            };
            if needs_fallback {
                if let Some(o) = data.borrow().nodes.first().copied() {
                    dispose_owner(o);
                }
                let row_owner = with_rt(|rt| create_owner(rt, None, false));
                let value = crate::run_with_owner(Some(Owner(row_owner)), || {
                    let d = data.borrow();
                    (d.fallback.as_ref().expect("checked"))()
                });
                let mut d = data.borrow_mut();
                d.nodes = vec![row_owner];
                d.mappings = Rc::new(vec![value]);
            }
            return;
        }

        let (offset, to, prev_to) = {
            let d = data.borrow();
            (d.offset, from + new_len, d.offset + d.len)
        };
        // Retained overlap [keep_start, keep_end) in global indices; empty when
        // the windows are disjoint, or when coming from empty/fallback.
        let keep_start = from.max(offset);
        let keep_end = to.min(prev_to);

        let mut mappings: Vec<Option<M>> = (0..new_len).map(|_| None).collect();
        let mut nodes: Vec<Option<NodeId>> = vec![None; new_len];

        if old_len != 0 {
            let d = data.borrow();
            let mut i = keep_start;
            while i < keep_end {
                nodes[i - from] = Some(d.nodes[i - offset]);
                mappings[i - from] = Some(d.mappings[i - offset].clone());
                i += 1;
            }
        }

        for i in from..to {
            if old_len != 0 && i >= keep_start && i < keep_end {
                continue;
            }
            let row_owner = with_rt(|rt| create_owner(rt, None, false));
            nodes[i - from] = Some(row_owner);
            let value = crate::run_with_owner(Some(Owner(row_owner)), || {
                let mut d = data.borrow_mut();
                (d.map)(i)
            });
            mappings[i - from] = Some(value);
        }

        // Commit: dispose the previous fallback, or the rows leaving the window.
        if old_len == 0 {
            if let Some(o) = data.borrow().nodes.first().copied() {
                dispose_owner(o);
            }
        } else {
            let departing: Vec<NodeId> = {
                let d = data.borrow();
                (offset..prev_to)
                    .filter(|i| *i < from || *i >= to)
                    .map(|i| d.nodes[i - offset])
                    .collect()
            };
            for o in departing {
                dispose_owner(o);
            }
        }

        let mut d = data.borrow_mut();
        d.mappings = Rc::new(mappings.into_iter().map(|m| m.expect("committed")).collect());
        d.nodes = nodes.into_iter().map(|n| n.expect("committed")).collect();
        d.offset = from;
        d.len = new_len;
    });

    let out = data.borrow().mappings.clone();
    out
}
