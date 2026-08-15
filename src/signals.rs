//! The public API — port of the synchronous surface of `src/signals.ts`.
//!
//! # Handles are `Copy`
//!
//! `ReadSignal`, `WriteSignal`, `Memo` and `Owner` are `Copy` keys into the
//! arena, so they drop into closures without ceremony. The cost of that choice
//! is that the runtime cannot tell whether user code can still name a node, so
//! arena slots are never reclaimed — see README.md ("Deviations").
//!
//! # Errors are values, not throws
//!
//! Upstream throws out of computations and reads. Here a computation returns
//! `Result` and a read has a `try_get`. `get()` panics on an errored read for
//! the same reason upstream throws: a stale value would be a lie. Reach for
//! `try_get` wherever the error is expected.

use std::any::Any;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::constants::EffectKind;
use crate::core::{
    create_computed, create_signal_node, current_value, read_node, refresh_node, set_signal,
    suppress_computed_recompute, ComputedSpec, ReadOutcome, SignalSpec,
};
use crate::effect::{create_effect_node, EffectSpec};
use crate::async_source::Async;
use crate::error::{ReactiveError, UserError};
use crate::node::{AnyValue, ComputeOutcome, Disposable, EqualsFn, NodeId};
use crate::owner::{create_owner, dispose, dispose_children};
use crate::runtime::{with_rt, Runtime};

// ------------------------------------------------------------------ options

/// A value comparator.
pub type Comparator<T> = Rc<dyn Fn(&T, &T) -> bool>;

/// The error arm of an effect bundle: receives the user's error and a function
/// that runs (and clears) the effect's pending cleanup.
pub type EffectErrorHandler = Box<dyn FnMut(UserError, &dyn Fn())>;

/// How a node decides whether a new value is a change.
pub enum Equals<T: 'static> {
    /// A comparator. `create_signal` / `create_memo` install `PartialEq`,
    /// which is the port of upstream's default `isEqual` (`a === b`).
    Fn(Comparator<T>),
    /// Upstream's `equals: false` — every write notifies.
    Never,
}

impl<T: 'static> std::fmt::Debug for Equals<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Equals::Fn(_) => f.write_str("Equals::Fn(..)"),
            Equals::Never => f.write_str("Equals::Never"),
        }
    }
}

impl<T: PartialEq + 'static> Default for Equals<T> {
    fn default() -> Self {
        Equals::Fn(Rc::new(|a: &T, b: &T| a == b))
    }
}

fn erase_equals<T: 'static>(e: Equals<T>) -> Option<EqualsFn> {
    match e {
        Equals::Never => None,
        Equals::Fn(f) => Some(Rc::new(move |a: &dyn Any, b: &dyn Any| {
            match (a.downcast_ref::<T>(), b.downcast_ref::<T>()) {
                (Some(a), Some(b)) => f(a, b),
                // Unreachable: a node's value type never changes. Treating a
                // mismatch as "changed" fails safe (notify) rather than
                // silently dropping an update.
                _ => false,
            }
        })),
    }
}

/// Options shared by signals and memos. Port of `SignalOptions` + `MemoOptions`.
pub struct Options<T: 'static> {
    equals: Equals<T>,
    name: &'static str,
    owned_write: bool,
    unobserved: Option<Rc<dyn Fn()>>,
    id: Option<Rc<str>>,
    transparent: bool,
    lazy: bool,
    sync: bool,
    loading_value: Option<T>,
}

impl<T: 'static> std::fmt::Debug for Options<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("equals", &self.equals)
            .field("name", &self.name)
            .field("owned_write", &self.owned_write)
            .field("id", &self.id)
            .field("transparent", &self.transparent)
            .field("lazy", &self.lazy)
            .field("sync", &self.sync)
            .finish_non_exhaustive()
    }
}

impl<T: PartialEq + 'static> Default for Options<T> {
    fn default() -> Self {
        Options::from_equals(Equals::default())
    }
}

impl<T: 'static> Options<T> {
    /// Start from an explicit equality rule.
    ///
    /// There is deliberately no `Options::new()`: the default is `PartialEq`,
    /// and a constructor that silently picked something else for non-`PartialEq`
    /// types would be a trap.
    pub fn from_equals(equals: Equals<T>) -> Self {
        Options {
            equals,
            name: "",
            owned_write: false,
            unobserved: None,
            id: None,
            transparent: false,
            lazy: false,
            sync: false,
            loading_value: None,
        }
    }

    /// Custom comparator.
    pub fn equals(f: impl Fn(&T, &T) -> bool + 'static) -> Self {
        Options::from_equals(Equals::Fn(Rc::new(f)))
    }

    /// Notify on every write, regardless of equality.
    pub fn never_equal() -> Self {
        Options::from_equals(Equals::Never)
    }

    /// Debug name (`dev` builds only).
    pub fn name(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    /// Suppress the dev-mode guard against writing inside an owned scope.
    pub fn owned_write(mut self, v: bool) -> Self {
        self.owned_write = v;
        self
    }

    /// Callback invoked when the node loses its last subscriber.
    pub fn unobserved(mut self, f: impl Fn() + 'static) -> Self {
        self.unobserved = Some(Rc::new(f));
        self
    }

    /// Stable identifier for the owner hierarchy.
    pub fn id(mut self, id: impl Into<Rc<str>>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Invisible to the hydration id scheme: inherits the parent's id instead
    /// of consuming a child slot.
    pub fn transparent(mut self, v: bool) -> Self {
        self.transparent = v;
        self
    }

    /// Defer the initial computation until first read, **and** opt into
    /// autodisposal — once the memo has no subscribers it is torn down and
    /// recomputed from scratch on the next read.
    pub fn lazy(mut self, v: bool) -> Self {
        self.lazy = v;
        self
    }

    /// Assert the computation is synchronous. Reserved for the async port;
    /// currently a no-op, since nothing here is asynchronous.
    pub fn sync(mut self, v: bool) -> Self {
        self.sync = v;
        self
    }

    /// Commit #0: a committed value the memo is born with, shown until the
    /// computation's first real answer lands.
    pub fn loading_value(mut self, v: T) -> Self {
        self.loading_value = Some(v);
        self
    }
}

/// Options for effect primitives. Port of `EffectOptions`.
#[derive(Default, Clone, Copy, Debug)]
pub struct EffectOptions {
    name: &'static str,
    defer: bool,
    schedule: bool,
    transparent: bool,
}

impl EffectOptions {
    pub fn new() -> Self {
        Self::default()
    }
    /// Defer the initial effect execution until the next change.
    pub fn defer(mut self, v: bool) -> Self {
        self.defer = v;
        self
    }
    /// Enqueue the initial callback through the effect queue instead of
    /// running it synchronously at creation.
    pub fn schedule(mut self, v: bool) -> Self {
        self.schedule = v;
        self
    }
    pub fn name(mut self, v: &'static str) -> Self {
        self.name = v;
        self
    }
    pub fn transparent(mut self, v: bool) -> Self {
        self.transparent = v;
        self
    }
}

// ------------------------------------------------------------------ handles

macro_rules! node_handle {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        pub struct $name<T: 'static> {
            id: NodeId,
            _t: PhantomData<fn() -> T>,
        }
        impl<T: 'static> Clone for $name<T> {
            fn clone(&self) -> Self {
                *self
            }
        }
        impl<T: 'static> Copy for $name<T> {}
        /// Identity comparison, matching upstream's `===` on the accessor's
        /// node. Two handles are equal when they name the same node — this is
        /// not a comparison of the values they hold.
        impl<T: 'static> PartialEq for $name<T> {
            fn eq(&self, other: &Self) -> bool {
                self.id == other.id
            }
        }
        impl<T: 'static> Eq for $name<T> {}
        impl<T: 'static> std::hash::Hash for $name<T> {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                self.id.hash(state)
            }
        }
        impl<T: 'static> $name<T> {
            pub(crate) fn from_id(id: NodeId) -> Self {
                $name {
                    id,
                    _t: PhantomData,
                }
            }
            /// The underlying arena key. Escape hatch for integrations.
            pub fn node_id(&self) -> NodeId {
                self.id
            }
        }
        impl<T: 'static> std::fmt::Debug for $name<T> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_tuple(stringify!($name)).field(&self.id).finish()
            }
        }
    };
}

node_handle!(ReadSignal, "Read half of a signal. A zero-arg getter: reading it inside a tracking scope subscribes that scope to changes.");
node_handle!(WriteSignal, "Write half of a signal.");
node_handle!(Memo, "A read-only derived value. Recomputes only when a tracked dependency actually changes.");

/// A reactive owner: the lifecycle node that cleanups and child primitives
/// attach to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Owner(pub(crate) NodeId);

impl Owner {
    pub(crate) fn id(self) -> NodeId {
        self.0
    }

    /// True once the owner has been disposed (or marked zombie pending
    /// disposal). Pair with a captured owner to bail out of late callbacks.
    pub fn is_disposed(self) -> bool {
        with_rt(|rt| crate::owner::is_disposed(rt, self.0))
    }

    /// The underlying arena key. Escape hatch for integrations and for the
    /// observational helpers in [`debug`].
    pub fn node_id(self) -> NodeId {
        self.0
    }

    /// The enclosing owner, if any.
    pub fn parent(self) -> Option<Owner> {
        with_rt(|rt| {
            let nodes = rt.nodes();
            nodes.get(self.0).and_then(|n| n.parent.get()).map(Owner)
        })
    }

    /// This owner's hydration id, if it is inside an id-carrying tree.
    pub fn hydration_id(self) -> Option<Rc<str>> {
        with_rt(|rt| {
            let nodes = rt.nodes();
            let out = nodes.get(self.0).and_then(|n| n.id.borrow().clone());
            out
        })
    }
}

/// Structural read-outs of the graph.
///
/// These exist for diagnostics and for the ported tests that assert on graph
/// shape rather than on values. They are observational only — nothing here can
/// mutate the graph.
pub mod debug {
    use super::*;

    /// Number of live nodes in the arena.
    pub fn node_count() -> usize {
        with_rt(|rt| rt.node_count())
    }

    /// Number of live dependency edges.
    pub fn link_count() -> usize {
        with_rt(|rt| rt.link_count())
    }

    /// Does this node subscribe to anything?
    pub fn has_dependencies(id: NodeId) -> bool {
        with_rt(|rt| {
            let nodes = rt.nodes();
            nodes.get(id).map(|n| n.deps.get().is_some()).unwrap_or(false)
        })
    }

    /// Total nodes in an owner subtree, counting the owner itself.
    ///
    /// Exists for the upstream regression test that pins disposed row owners
    /// not accumulating in their parent's sibling chain.
    pub fn owner_subtree_size(id: NodeId) -> usize {
        with_rt(|rt| {
            let nodes = rt.nodes();
            fn walk(nodes: &crate::runtime::Nodes, id: NodeId) -> usize {
                let Some(n) = nodes.get(id) else { return 0 };
                let mut count = 1;
                let mut child = n.first_child.get();
                while let Some(c) = child {
                    count += walk(nodes, c);
                    child = match nodes.get(c) {
                        Some(cn) => cn.next_sibling.get(),
                        None => None,
                    };
                }
                count
            }
            walk(&nodes, id)
        })
    }

    /// How many subscribers does this node have?
    pub fn subscriber_count(id: NodeId) -> usize {
        with_rt(|rt| {
            let nodes = rt.nodes();
            let links = rt.links();
            let Some(n) = nodes.get(id) else { return 0 };
            let mut count = 0;
            let mut cur = n.subs.get();
            while let Some(lid) = cur {
                count += 1;
                cur = links[lid].next_sub.get();
            }
            count
        })
    }
}

/// The disposer handed to a [`create_root`] body.
#[derive(Clone, Copy, Debug)]
pub struct Dispose(NodeId);

impl Dispose {
    /// Tear down every signal, memo, effect and cleanup registered inside the
    /// root. Idempotent.
    pub fn dispose(self) {
        with_rt(|rt| dispose_children(rt, self.0, true, false));
    }
}

// --------------------------------------------------------------- read/write

fn read_value<T: 'static, R>(
    id: NodeId,
    project: impl FnOnce(Option<&T>) -> R,
) -> Result<R, ReactiveError> {
    // The value is cloned out (an `Rc` bump) so the projection — user code —
    // runs with no arena borrow held.
    let value: Result<Option<AnyValue>, ReactiveError> = with_rt(|rt| match read_node(rt, id) {
        ReadOutcome::Err(e) => Err(e),
        ReadOutcome::Value => Ok(current_value(rt, id)),
    });
    let value = value?;
    Ok(project(value.as_ref().and_then(|v| v.downcast_ref::<T>())))
}

macro_rules! readable {
    ($name:ident) => {
        impl<T: 'static> $name<T> {
            /// Read the value, subscribing the current tracking scope.
            ///
            /// # Panics
            ///
            /// If the computation is in an error state — upstream throws in the
            /// same situation. Use [`Self::try_get`] where that is expected.
            #[track_caller]
            pub fn get(&self) -> T
            where
                T: Clone,
            {
                match self.try_get() {
                    Ok(v) => v,
                    Err(e) => panic!("solid_rs: read of an errored node: {}", e.message()),
                }
            }

            /// Read the value, surfacing an errored computation instead of
            /// panicking.
            pub fn try_get(&self) -> Result<T, ReactiveError>
            where
                T: Clone,
            {
                read_value::<T, _>(self.id, |v| {
                    v.cloned()
                        .expect("solid_rs: node has no value; read before initialisation")
                })
            }

            /// Read by reference, for values that are not `Clone`.
            ///
            /// `f` runs while the runtime is *not* borrowed, so creating
            /// reactive primitives inside it is safe.
            #[track_caller]
            pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
                match read_value::<T, _>(self.id, |v| {
                    v.map(f)
                        .expect("solid_rs: node has no value; read before initialisation")
                }) {
                    Ok(v) => v,
                    Err(e) => panic!("solid_rs: read of an errored node: {}", e.message()),
                }
            }

            /// Invalidate this source, forcing it to re-execute even though its
            /// inputs have not changed.
            ///
            /// A write-like operation: it does not read the target. Refreshing
            /// a plain signal is a no-op, matching upstream.
            pub fn refresh(&self) {
                with_rt(|rt| refresh_node(rt, self.id));
            }
        }
    };
}

readable!(ReadSignal);
readable!(Memo);

impl<T: 'static> WriteSignal<T> {
    /// Replace the value.
    ///
    /// The write stages: readers outside a tracking scope see the old value
    /// until the next flush. That is the 2.0 model — a tick is the unit of
    /// truth for values — not a bug.
    pub fn set(&self, value: T) {
        self.write(move |_| Rc::new(value) as AnyValue);
    }

    /// Derive the next value from the current one.
    pub fn update(&self, f: impl FnOnce(&T) -> T) {
        self.write(move |prev| {
            let prev = prev
                .and_then(|p| p.downcast_ref::<T>())
                .expect("solid_rs: update() on a node with no value");
            Rc::new(f(prev)) as AnyValue
        });
    }

    /// Upstream splits this in two: `setSignal` for a plain signal and
    /// `setMemo` (write **plus** suppress the queued recompute) for the
    /// writable-memo form of `createSignal(fn)`.
    ///
    /// Here both forms share one setter type, so the branch is taken on the
    /// node instead of on the call site. That cannot be misapplied — only a
    /// writable memo is a computed — and it keeps `create_signal` and
    /// `create_computed_signal` returning the same setter type.
    fn write(&self, make: impl FnOnce(Option<&dyn Any>) -> AnyValue) {
        with_rt(|rt| {
            set_signal(rt, self.id, make);
            let is_computed = {
                let nodes = rt.nodes();
                nodes.get(self.id).map(|n| n.is_computed()).unwrap_or(false)
            };
            if is_computed {
                suppress_computed_recompute(rt, self.id);
            }
        });
    }
}

// ------------------------------------------------------------- constructors

/// Create a signal with `PartialEq` change detection.
///
/// ```
/// use solid_rs::{create_signal, flush};
/// let (count, set_count) = create_signal(0);
/// assert_eq!(count.get(), 0);
/// set_count.set(1);
/// flush();
/// assert_eq!(count.get(), 1);
/// ```
pub fn create_signal<T: PartialEq + 'static>(value: T) -> (ReadSignal<T>, WriteSignal<T>) {
    create_signal_with(value, Options::default())
}

/// Create a signal with explicit options.
pub fn create_signal_with<T: 'static>(
    value: T,
    options: Options<T>,
) -> (ReadSignal<T>, WriteSignal<T>) {
    let id = with_rt(|rt| {
        create_signal_node(
            rt,
            SignalSpec {
                value: Some(Rc::new(value)),
                equals: erase_equals(options.equals),
                unobserved: options.unobserved,
                owned_write: options.owned_write,
                name: if options.name.is_empty() {
                    "signal"
                } else {
                    options.name
                },
            },
        )
    });
    (ReadSignal::from_id(id), WriteSignal::from_id(id))
}

fn erase_compute<T: 'static>(
    mut f: impl FnMut(Option<&T>) -> Result<T, ReactiveError> + 'static,
) -> crate::node::ComputeFn {
    Box::new(move |prev: Option<&dyn Any>| {
        let prev = prev.and_then(|p| p.downcast_ref::<T>());
        f(prev).map(|v| ComputeOutcome::Value(Rc::new(v) as AnyValue))
    })
}

fn memo_spec<T: 'static>(
    compute: crate::node::ComputeFn,
    options: Options<T>,
    effect_kind: Option<EffectKind>,
    default_name: &'static str,
) -> ComputedSpec {
    ComputedSpec {
        compute,
        equals: erase_equals(options.equals),
        unobserved: options.unobserved,
        id: options.id,
        name: if options.name.is_empty() {
            default_name
        } else {
            options.name
        },
        transparent: options.transparent,
        owned_write: options.owned_write,
        lazy: options.lazy,
        sync: options.sync,
        loading_value: options.loading_value.map(|v| Rc::new(v) as AnyValue),
        effect_kind,
    }
}

/// Create a read-only derived value.
///
/// ```
/// use solid_rs::{create_signal, create_memo, flush};
/// let (first, set_first) = create_signal("Ada".to_string());
/// let greeting = create_memo(move |_| format!("Hello, {}", first.get()));
/// assert_eq!(greeting.get(), "Hello, Ada");
/// set_first.set("Grace".to_string());
/// flush();
/// assert_eq!(greeting.get(), "Hello, Grace");
/// ```
pub fn create_memo<T: PartialEq + 'static>(
    compute: impl FnMut(Option<&T>) -> T + 'static,
) -> Memo<T> {
    create_memo_with(compute, Options::default())
}

/// Create a read-only derived value with explicit options.
pub fn create_memo_with<T: 'static>(
    mut compute: impl FnMut(Option<&T>) -> T + 'static,
    options: Options<T>,
) -> Memo<T> {
    try_create_memo_with(move |prev| Ok(compute(prev)), options)
}

/// Create a derived value whose computation may fail.
///
/// An `Err` puts the memo into the error state: it propagates to dependents by
/// identity, and a read surfaces it rather than serving a stale value.
pub fn try_create_memo_with<T: 'static>(
    compute: impl FnMut(Option<&T>) -> Result<T, ReactiveError> + 'static,
    options: Options<T>,
) -> Memo<T> {
    let id = with_rt(|rt| {
        let spec = memo_spec(erase_compute(compute), options, None, "memo");
        create_computed(rt, spec)
    });
    Memo::from_id(id)
}

/// Create a derived value whose computation may be asynchronous.
///
/// The computation returns an [`Async<T>`]: a value that is already available,
/// a [`Flight`] that will settle, or a [`Streaming`] sequence. While a flight is
/// in flight the memo is *suspended* — a tracked read raises `NotReadyError`
/// into the graph, which is what a loading boundary catches, and an untracked
/// read surfaces it through [`Memo::try_get`].
///
/// The first landing is *loading*, not pending: there is no observable value
/// yet to be non-final, so [`is_pending`] reads `false`. A later refetch — a new
/// question with a stale value still showing — is pending.
///
/// ```
/// use solid_rs::*;
/// let (flight, resolver) = Flight::<i32>::new();
/// let data = create_root(move |_| {
///     let f = flight.clone();
///     create_async_memo(move |_| Async::Flight(f.clone()))
/// });
///
/// // Suspended: the read raises rather than inventing a value.
/// assert!(data.try_get().is_err());
///
/// resolver.resolve(42);
/// host::drive_microtasks();
/// assert_eq!(data.get(), 42);
/// ```
pub fn create_async_memo<T: PartialEq + 'static>(
    compute: impl FnMut(Option<&T>) -> Async<T> + 'static,
) -> Memo<T> {
    create_async_memo_with(compute, Options::default())
}

/// [`create_async_memo`] with explicit options.
///
/// `Options::loading_value(v)` gives the memo commit #0: it is born committed
/// with `v`, so its first flight suspends nothing and stays verdict-quiet (A27).
pub fn create_async_memo_with<T: 'static>(
    mut compute: impl FnMut(Option<&T>) -> Async<T> + 'static,
    options: Options<T>,
) -> Memo<T> {
    let erased: crate::node::ComputeFn = Box::new(move |prev: Option<&dyn Any>| {
        let prev = prev.and_then(|p| p.downcast_ref::<T>());
        Ok(match compute(prev) {
            Async::Ready(v) => ComputeOutcome::Value(Rc::new(v) as AnyValue),
            Async::Flight(f) => ComputeOutcome::Flight(Rc::new(f)),
            Async::Stream(s) => ComputeOutcome::Stream(Rc::new(s)),
        })
    });
    let id = with_rt(|rt| {
        let spec = memo_spec(erased, options, None, "asyncMemo");
        create_computed(rt, spec)
    });
    Memo::from_id(id)
}

/// Create a **writable** memo — upstream's `createSignal(fn)`.
///
/// The value starts as the computation's result and can be locally overwritten
/// by the setter; the manual write wins over a recompute queued in the same
/// tick, and a later upstream change recomputes normally.
pub fn create_computed_signal<T: PartialEq + 'static>(
    compute: impl FnMut(Option<&T>) -> T + 'static,
) -> (Memo<T>, WriteSignal<T>) {
    create_computed_signal_with(compute, Options::default())
}

/// [`create_computed_signal`] with explicit options.
pub fn create_computed_signal_with<T: 'static>(
    mut compute: impl FnMut(Option<&T>) -> T + 'static,
    options: Options<T>,
) -> (Memo<T>, WriteSignal<T>) {
    let id = with_rt(|rt| {
        let spec = memo_spec(
            erase_compute(move |prev| Ok(compute(prev))),
            options,
            None,
            "memo",
        );
        let id = create_computed(rt, spec);
        // A writable memo lives for its owner's lifetime rather than being
        // torn down when its subscriber count hits zero.
        let nodes = rt.nodes();
        let n = crate::runtime::node(&nodes, id);
        n.config
            .set(n.config.get() & !crate::constants::config::AUTO_DISPOSE);
        id
    });
    (Memo::from_id(id), ComputedWriter::from_id(id))
}

/// The setter half of a writable memo routes through `set_memo`, which also
/// suppresses the queued recompute.
type ComputedWriter<T> = WriteSignal<T>;

// ------------------------------------------------------------------ effects

/// What an effect's apply phase may return.
///
/// Implemented for `()` (no cleanup) and `Option<Disposable>`. Use
/// [`on_dispose`] to build the latter.
pub trait IntoCleanup {
    fn into_cleanup(self) -> Option<Disposable>;
}

impl IntoCleanup for () {
    fn into_cleanup(self) -> Option<Disposable> {
        None
    }
}

impl IntoCleanup for Option<Disposable> {
    fn into_cleanup(self) -> Option<Disposable> {
        self
    }
}

/// Wrap a closure as an effect cleanup.
///
/// ```
/// use solid_rs::{create_signal, create_effect, create_root, on_dispose, flush};
/// let (n, _set) = create_signal(0);
/// create_root(move |_| {
///     create_effect(move |_| n.get(), move |_v: &i32, _| on_dispose(|| { /* teardown */ }));
/// });
/// flush();
/// ```
pub fn on_dispose(f: impl FnOnce() + 'static) -> Option<Disposable> {
    Some(Box::new(f))
}

/// Create a reactive effect with separate compute and apply phases.
///
/// `compute` runs tracked — put reactive reads there. `apply` runs untracked
/// after the queue flushes — put side effects there. Reads inside `apply` do
/// **not** re-trigger the effect; that split is the point.
///
/// ```
/// use solid_rs::{create_signal, create_effect, create_root, flush};
/// use std::{cell::RefCell, rc::Rc};
/// let log = Rc::new(RefCell::new(Vec::new()));
/// let sink = log.clone();
/// let (count, set_count) = create_signal(0);
/// create_root(move |_| {
///     create_effect(move |_| count.get(), move |v: &i32, _| sink.borrow_mut().push(*v));
/// });
/// flush();
/// set_count.set(1);
/// flush();
/// assert_eq!(*log.borrow(), vec![0, 1]);
/// ```
pub fn create_effect<T: 'static, R: IntoCleanup + 'static>(
    compute: impl FnMut(Option<&T>) -> T + 'static,
    apply: impl FnMut(&T, Option<&T>) -> R + 'static,
) {
    create_effect_with(compute, apply, EffectOptions::default());
}

/// [`create_effect`] with explicit options.
pub fn create_effect_with<T: 'static, R: IntoCleanup + 'static>(
    mut compute: impl FnMut(Option<&T>) -> T + 'static,
    mut apply: impl FnMut(&T, Option<&T>) -> R + 'static,
    options: EffectOptions,
) {
    build_effect(
        erase_compute(move |prev| Ok(compute(prev))),
        erase_apply(move |v: &T, p| Ok(apply(v, p).into_cleanup())),
        None,
        EffectKind::User,
        options,
    );
}

/// A user effect whose compute phase may fail, with an error arm.
///
/// The error arm is the error arm of the *effect phase*: it runs on the same
/// queue, in the same imperative, writable scope as `apply`, and only for
/// settled errors — an error that recovers before the effect phase runs
/// `apply` instead.
pub fn try_create_effect<T: 'static, R: IntoCleanup + 'static>(
    compute: impl FnMut(Option<&T>) -> Result<T, ReactiveError> + 'static,
    mut apply: impl FnMut(&T, Option<&T>) -> Result<R, ReactiveError> + 'static,
    on_error: Option<EffectErrorHandler>,
    options: EffectOptions,
) {
    build_effect(
        erase_compute(compute),
        erase_apply(move |v: &T, p| apply(v, p).map(IntoCleanup::into_cleanup)),
        on_error,
        EffectKind::User,
        options,
    );
}

/// Create a reactive computation that runs during the render phase.
///
/// Same compute/apply split as [`create_effect`], but scheduled in the render
/// queue rather than after it. Reach for this only when writing renderer
/// plumbing; application code wants [`create_effect`].
pub fn create_render_effect<T: 'static, R: IntoCleanup + 'static>(
    mut compute: impl FnMut(Option<&T>) -> T + 'static,
    mut apply: impl FnMut(&T, Option<&T>) -> R + 'static,
) {
    build_effect(
        erase_compute(move |prev| Ok(compute(prev))),
        erase_apply(move |v: &T, p| Ok(apply(v, p).into_cleanup())),
        None,
        EffectKind::Render,
        EffectOptions::default(),
    );
}

/// [`create_render_effect`] with explicit options.
pub fn create_render_effect_with<T: 'static, R: IntoCleanup + 'static>(
    mut compute: impl FnMut(Option<&T>) -> T + 'static,
    mut apply: impl FnMut(&T, Option<&T>) -> R + 'static,
    options: EffectOptions,
) {
    build_effect(
        erase_compute(move |prev| Ok(compute(prev))),
        erase_apply(move |v: &T, p| Ok(apply(v, p).into_cleanup())),
        None,
        EffectKind::Render,
        options,
    );
}

fn erase_apply<T: 'static>(
    mut f: impl FnMut(&T, Option<&T>) -> Result<Option<Disposable>, ReactiveError> + 'static,
) -> crate::node::EffectFn {
    Box::new(move |v: &dyn Any, p: Option<&dyn Any>| {
        let v = v
            .downcast_ref::<T>()
            .expect("solid_rs: effect value type mismatch");
        f(v, p.and_then(|p| p.downcast_ref::<T>()))
    })
}

fn build_effect(
    compute: crate::node::ComputeFn,
    apply: crate::node::EffectFn,
    on_error: Option<EffectErrorHandler>,
    kind: EffectKind,
    options: EffectOptions,
) -> NodeId {
    with_rt(|rt| {
        create_effect_node(
            rt,
            EffectSpec {
                spec: ComputedSpec {
                    compute,
                    equals: None, // effects use `equals: false`
                    unobserved: None,
                    id: None,
                    name: if options.name.is_empty() {
                        "effect"
                    } else {
                        options.name
                    },
                    transparent: options.transparent,
                    owned_write: false,
                    lazy: true,
                    sync: false,
                    loading_value: None,
                    effect_kind: Some(kind),
                },
                effect_fn: apply,
                error_fn: on_error,
                defer: options.defer,
                schedule: options.schedule,
            },
        )
    })
}

/// Create a reaction: a one-shot effect that fires when the tracked expression
/// invalidates, and must be re-armed to fire again.
///
/// Each call to the returned `track` **replaces** the previous arm (1.x
/// semantics): without disposing the superseded arm its sources would stay
/// live, each accumulated arm would deliver its own fire, and un-fired arms
/// would leak as live effect nodes.
///
/// ```
/// use solid_rs::{create_signal, create_reaction, create_root, flush};
/// use std::{cell::Cell, rc::Rc};
/// let count = Rc::new(Cell::new(0));
/// let c = count.clone();
/// let (sign, set_sign) = create_signal(0);
/// let track = create_root(move |_| create_reaction(move || c.set(c.get() + 1)));
/// track(Box::new(move || { sign.get(); }));
/// set_sign.set(1);
/// flush();
/// assert_eq!(count.get(), 1);
/// ```
pub fn create_reaction(
    on_invalidate: impl FnMut() + 'static,
) -> impl Fn(Box<dyn Fn()>) + 'static {
    let owner = crate::get_owner();
    // The currently armed effect node. `track()` replaces the previous
    // subscription; without disposing the superseded arm its sources stay live
    // (firing the callback for replaced dependencies), each accumulated arm
    // delivers its own fire, and un-fired arms leak as live effect nodes
    // (upstream #2861).
    let arm: Rc<std::cell::Cell<Option<NodeId>>> = Rc::new(std::cell::Cell::new(None));
    let handler: Rc<std::cell::RefCell<dyn FnMut()>> =
        Rc::new(std::cell::RefCell::new(on_invalidate));

    let arm_outer = arm.clone();
    move |tracking: Box<dyn Fn()>| {
        if let Some(prev) = arm_outer.take() {
            with_rt(|rt| dispose(rt, prev));
        }
        let arm_inner = arm_outer.clone();
        let arm_apply = arm_outer.clone();
        let handler = handler.clone();
        crate::run_with_owner(owner, move || {
            build_effect(
                // The compute both arms the tracking and yields the node's own
                // id as its value, so the apply phase can dispose it — the same
                // trick upstream uses (`() => (tracking(), (arm = getOwner()))`).
                Box::new(move |_prev| {
                    tracking();
                    let self_id = with_rt(|rt| rt.ctx.get());
                    arm_inner.set(self_id);
                    Ok(ComputeOutcome::Value(Rc::new(self_id) as AnyValue))
                }),
                Box::new(move |v, _p| {
                    arm_apply.set(None);
                    handler.borrow_mut()();
                    // Fire-once: the arm disposes itself after delivering, so a
                    // later change needs a fresh `track()`.
                    if let Some(id) = v.downcast_ref::<Option<NodeId>>().copied().flatten() {
                        with_rt(|rt| dispose(rt, id));
                    }
                    Ok(None)
                }),
                None,
                EffectKind::User,
                EffectOptions::default().defer(true),
            );
        });
    }
}

// ------------------------------------------------------------------- owners

/// Create a detached reactive root.
///
/// The callback receives a disposer which tears down every signal, memo,
/// effect and cleanup registered inside.
///
/// ```
/// use solid_rs::{create_signal, create_memo, create_root, flush};
/// let d = create_root(|dispose| {
///     let (n, _set) = create_signal(1);
///     let _m = create_memo(move |_| n.get() + 1);
///     dispose
/// });
/// d.dispose();
/// ```
pub fn create_root<T>(init: impl FnOnce(Dispose) -> T) -> T {
    create_root_with(init, None, false)
}

/// [`create_root`] with an explicit hydration id and transparency.
pub fn create_root_with<T>(
    init: impl FnOnce(Dispose) -> T,
    id: Option<Rc<str>>,
    transparent: bool,
) -> T {
    let owner = with_rt(|rt| create_owner(rt, id, transparent));
    crate::run_with_owner(Some(Owner(owner)), || init(Dispose(owner)))
}

/// Register a callback that runs when the surrounding owner is disposed.
///
/// Outside an owner this is a no-op, matching upstream.
pub fn on_cleanup(f: impl FnOnce() + 'static) {
    with_rt(|rt| {
        let registered = crate::owner::cleanup(rt, Box::new(f));
        #[cfg(feature = "dev")]
        if !registered {
            eprintln!(
                "[NO_OWNER_CLEANUP] on_cleanup called outside a reactive context will never be run"
            );
        }
        #[cfg(not(feature = "dev"))]
        let _ = registered;
    });
}

/// The current reactive owner — the lifecycle node the next `on_cleanup` or
/// primitive attaches to.
pub fn get_owner() -> Option<Owner> {
    with_rt(|rt| rt.ctx.get().map(Owner))
}

/// The currently-tracking observer, or `None` if reads here would be
/// untracked.
///
/// Application code rarely needs this — see [`get_owner`] for the lifecycle
/// owner instead.
pub fn get_observer() -> Option<Owner> {
    with_rt(|rt| {
        if rt.tracking.get() {
            rt.ctx.get().map(Owner)
        } else {
            None
        }
    })
}

/// Access the runtime for a diagnostic read. Used by the reclamation tests.
pub fn live_node_count() -> usize {
    with_rt(|rt: &Runtime| rt.node_count())
}
