//! Port of `src/core/types.ts` and `src/core/graph.ts`'s `Link`.
//!
//! # Shape change from the TypeScript
//!
//! Upstream has three overlapping object shapes — `RawSignal`, `Owner`, and
//! `Computed extends RawSignal, Owner` — plus an `Effect` that adds fields on
//! top of `Computed`. Which shape an object has is discovered structurally at
//! runtime (`(el as any)._fn`, `(el as Root)._root`, `(el as any)._type`).
//!
//! Rust has no structural typing, and splitting the arena by shape would mean
//! three key spaces and a tag on every edge. Instead there is **one** `Node`
//! with a [`NodeKind`] discriminant and the union of the fields. That keeps
//! every edge a single `NodeId`, keeps the ported algorithms line-comparable
//! with the TypeScript, and turns upstream's structural probes into cheap
//! enum/`Option` tests:
//!
//! | TypeScript                | here                        |
//! |---------------------------|-----------------------------|
//! | `(el as any)._fn`         | `node.is_computed()`        |
//! | `(el as Root)._root`      | `node.kind == NodeKind::Root` |
//! | `(el as any)._type`       | `node.effect_kind.get()`    |
//!
//! # Interior mutability
//!
//! Fields carry their own `Cell`/`RefCell` rather than the arena handing out
//! `&mut Node`. This is deliberate: the graph algorithms are recursive walks
//! that mutate one node while iterating another's link list, which `&mut`
//! aliasing forbids outright. With per-field cells a single shared borrow of
//! the arena covers a whole walk.
//!
//! The invariant that keeps this sound and panic-free is stated once, in
//! [`crate::runtime`]: an arena borrow may span engine-internal work, but must
//! be dropped before any user callback runs or any node is created.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use slotmap::new_key_type;

use crate::constants::EffectKind;
use crate::error::{ReactiveError, UserError};

new_key_type! {
    /// Generational handle for a reactive node.
    pub struct NodeId;
    /// Generational handle for one dependency edge.
    pub struct LinkId;
    /// Generational handle for an effect queue.
    pub struct QueueId;
    /// Generational handle for a transaction (`Transition` upstream).
    pub struct TransitionId;
}

/// A type-erased reactive value.
///
/// `Rc`, not `Box`, for two reasons that both come straight from the port:
/// upstream stores the *same* JS object in `_value` and `_prevValue` after an
/// effect run, which a `Box` cannot express; and every path that hands a value
/// to user code must first release the arena borrow, which a cheap clone makes
/// trivial. This is not refcounting the graph — values are acyclic payloads,
/// and the edges remain generational keys.
pub type AnyValue = Rc<dyn Any>;

/// A cleanup callback returned from an effect body, or registered with
/// `on_cleanup`. Runs exactly once.
pub type Disposable = Box<dyn FnOnce()>;

/// A computation. Receives the previous value (absent on the first run) and
/// returns the next one, or the error the computation failed with.
///
/// Upstream signals failure by throwing. Rust models it on the return channel:
/// unwinding across the scheduler would have to be caught at every boundary
/// with `catch_unwind`, and a panic is not a recoverable value in the way the
/// graph needs (it must be *stored on the node*, propagated by identity, and
/// re-delivered to late readers). The semantics — which node owns the error,
/// how it propagates, when it recovers — are unchanged.
pub type ComputeFn = Box<dyn FnMut(Option<&dyn Any>) -> Result<ComputeOutcome, ReactiveError>>;

/// What a computation produced. Upstream discovers these three shapes by
/// probing the returned object (`Symbol.asyncIterator`, then `.then`); naming
/// them makes the `sync: true` contract a type rather than a runtime
/// diagnostic.
pub enum ComputeOutcome {
    /// A settled value.
    Value(AnyValue),
    /// A one-shot in-flight value.
    Flight(Rc<dyn ErasedFlight>),
    /// A sequence of landings.
    Stream(Rc<dyn ErasedStream>),
}

/// One landing of a stream, type-erased.
pub enum ErasedStep {
    Yield(AnyValue),
    Error(ReactiveError),
    Done,
}

/// Type-erased [`crate::Flight`], so the graph can hold flights of differing
/// value types behind one node shape.
pub trait ErasedFlight {
    /// Identity, for upstream's `el._inFlight !== result` supersession guard.
    fn flight_id(&self) -> usize;
    /// The value if the flight had already settled when the computation
    /// returned it — upstream's synchronously-resolved thenable.
    fn take_settled(&self) -> Option<Result<AnyValue, ReactiveError>>;
    fn on_settle(&self, f: Box<dyn FnOnce(Result<AnyValue, ReactiveError>)>);
}

/// Type-erased [`crate::Streaming`].
pub trait ErasedStream {
    fn stream_id(&self) -> usize;
    fn take_buffered(&self) -> Option<ErasedStep>;
    fn on_step(&self, f: Box<dyn Fn(ErasedStep)>);
    fn close(&self);
}

/// The apply phase of an effect. Receives `(next, prev)`, may return a
/// cleanup, and may fail.
///
/// The apply phase is the user's own imperative code, so upstream expects it
/// to `try/catch` where failures occur — an uncaught throw there is an
/// unhandled application error that escalates to the nearest error boundary,
/// or halts the system if there is none. The `Result` here is that same
/// escalation channel, made explicit.
pub type EffectFn =
    Box<dyn FnMut(&dyn Any, Option<&dyn Any>) -> Result<Option<Disposable>, ReactiveError>>;

/// The error arm of an effect bundle. Receives the user's error and a function
/// that runs (and clears) the effect's pending cleanup.
pub type ErrorFn = Box<dyn FnMut(UserError, &dyn Fn())>;

/// A cached queue callback that re-runs one effect. Port of `_boundRunEffect`.
pub type EffectRunner = Rc<dyn Fn(u8)>;

/// Port of `_notifyStatus`: intercepts status propagation for a node.
pub type StatusNotifier = Rc<dyn Fn(Option<u32>, Option<ReactiveError>)>;

/// A stored computation, shared so it can be cloned out of the arena and
/// invoked with no borrow held.
pub type SharedCompute = Rc<RefCell<ComputeFn>>;

/// Type-erased equality. `None` in [`Node::equals`] is upstream's
/// `equals: false` — always notify.
pub type EqualsFn = Rc<dyn Fn(&dyn Any, &dyn Any) -> bool>;

/// Identifier for a context channel. Port of the `Symbol()` upstream uses.
pub type ContextId = u64;

/// Port of `Owner._context`. Upstream copies the record on write
/// (`{...owner._context, [id]: value}`) and shares it by reference on node
/// creation, so a child's write is invisible to its parent. `Rc` + clone-on-
/// write reproduces exactly that, and keeps creation O(1).
pub type ContextRecord = Rc<HashMap<ContextId, Rc<dyn Any>>>;

/// Which of upstream's three overlapping shapes this node is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    /// `RawSignal` — a value with subscribers and no computation.
    Signal,
    /// `Computed` — a signal that is also an owner and has a computation.
    /// Effects are computeds with `effect_kind` set.
    Computed,
    /// `Root` — an owner with no value and no computation.
    Root,
}

/// One dependency edge. Port of `interface Link`.
///
/// Edges are intrusive doubly-linked in two directions at once — along the
/// subscriber's dep list (`next_dep`) and along the dependency's sub list
/// (`prev_sub`/`next_sub`) — exactly as upstream, so unlinking is O(1).
pub struct Link {
    pub dep: Cell<NodeId>,
    pub sub: Cell<NodeId>,
    pub next_dep: Cell<Option<LinkId>>,
    pub prev_sub: Cell<Option<LinkId>>,
    pub next_sub: Cell<Option<LinkId>>,
    /// `sub.dep_gen` at the time this link was created or last revalidated
    /// in-order. A link stamped with the subscriber's current pass generation
    /// is inside the validated `[deps..deps_tail]` prefix — an O(1)
    /// replacement for scanning the dep list to answer membership.
    pub gen: Cell<u32>,
    /// True when the link was created by an `is_pending` read. Such a link
    /// observes the dep's pending state only. Nothing in the synchronous core
    /// sets it; the flag and the branches that read it are ported because the
    /// verdict layer (out of scope) is what sets it.
    pub pending_observer: Cell<bool>,
}

/// A reactive node: signal, computed/effect, or root owner.
pub struct Node {
    // ---------------------------------------------------------------- identity
    pub kind: NodeKind,
    /// Stable hydration id. `None` outside an id-carrying tree.
    pub id: RefCell<Option<Rc<str>>>,
    /// Debug name. Present only under the `dev` feature, mirroring upstream's
    /// `__DEV__`-only `_name`.
    #[cfg(feature = "dev")]
    pub name: Cell<&'static str>,
    /// Static configuration bits (`config::*`).
    pub config: Cell<u32>,

    // ------------------------------------------------------------ signal half
    pub value: RefCell<Option<AnyValue>>,
    /// Staged value. `None` is upstream's `NOT_PENDING` sentinel: plain writes
    /// stage here and commit to `value` at flush, which is what makes a tick
    /// the unit of truth for values.
    pub pending_value: RefCell<Option<AnyValue>>,
    /// `None` is upstream's `equals: false`.
    pub equals: RefCell<Option<EqualsFn>>,
    pub unobserved: RefCell<Option<Rc<dyn Fn()>>>,
    pub subs: Cell<Option<LinkId>>,
    pub subs_tail: Cell<Option<LinkId>>,
    pub time: Cell<u64>,
    pub transition: Cell<Option<TransitionId>>,

    // ------------------------------------------------------------- owner half
    pub parent: Cell<Option<NodeId>>,
    pub first_child: Cell<Option<NodeId>>,
    pub next_sibling: Cell<Option<NodeId>>,
    pub prev_sibling: Cell<Option<NodeId>>,
    /// Children detached by a rerun, awaiting zombie disposal at commit.
    pub pending_first_child: Cell<Option<NodeId>>,
    pub disposal: RefCell<Vec<Disposable>>,
    pub pending_disposal: RefCell<Vec<Disposable>>,
    /// Effect-returned cleanup; managed across reruns, invoked at true
    /// disposal.
    pub cleanup: RefCell<Option<Disposable>>,
    pub context: RefCell<ContextRecord>,
    pub child_count: Cell<u32>,
    pub queue: Cell<QueueId>,
    /// `Root._parentComputed`: the nearest enclosing computed, skipping roots.
    /// Roots are owners but not graph nodes, so height and linking walk past
    /// them.
    pub root_parent_computed: Cell<Option<NodeId>>,

    // ---------------------------------------------------------- computed half
    pub compute: RefCell<Option<SharedCompute>>,
    pub deps: Cell<Option<LinkId>>,
    pub deps_tail: Cell<Option<LinkId>>,
    /// Recompute-pass counter; bumped when dep revalidation starts.
    pub dep_gen: Cell<u32>,
    pub flags: Cell<u32>,
    pub status_flags: Cell<u32>,
    pub height: Cell<i32>,
    pub next_heap: Cell<Option<NodeId>>,
    /// Tail pointer of the circular bucket list. Upstream initialises it to
    /// the node itself; we do the same once the key is known.
    pub prev_heap: Cell<NodeId>,
    pub error: RefCell<Option<ReactiveError>>,
    pub blocked: Cell<bool>,
    pub pending_sources: RefCell<Option<HashSet<NodeId>>>,
    /// Question-scoped pending classification of the node's current pending
    /// window. Meaningless while not `status::PENDING`.
    pub reask: Cell<bool>,
    /// True while a `loading_value` node's first real answer hasn't landed.
    pub loading: Cell<bool>,
    /// Set by boundary computeds (out of scope) to intercept status
    /// propagation, and by effects to route their own status. Port of
    /// `_notifyStatus`.
    pub notify_status: RefCell<Option<StatusNotifier>>,
    /// Head of this computed's firewall-signal chain (`_child`), and the
    /// chain link (`_nextChild`) on a firewall signal.
    ///
    /// Firewall signals are created by the proxy store, which is a separate
    /// port — so both are always `None` here. The fields exist because
    /// `mark_node`, `for_each_dependent` and `update_if_necessary` all walk
    /// them, and porting those walks with the branch removed would leave the
    /// store port to re-derive where the branch belonged.
    pub child: Cell<Option<NodeId>>,
    pub next_child: Cell<Option<NodeId>>,
    /// Set on a firewall signal to point back at the computed behind it
    /// (`_firewall`). Always `None` here, for the same reason.
    pub firewall: Cell<Option<NodeId>>,
    /// Identity of the in-flight async result, or 0 for none. Port of
    /// `_inFlight`: a settle whose id no longer matches has been superseded by
    /// a newer computation and is dropped.
    pub in_flight: Cell<usize>,
    /// The live stream, held so the node keeps pulling across landings.
    pub in_flight_stream: RefCell<Option<Rc<dyn ErasedStream>>>,
    /// Companion: the boolean `is_pending` signal (`_pendingSignal`).
    pub pending_signal: Cell<Option<NodeId>>,
    /// Companion: the `latest()` shadow computed (`_latestValueComputed`).
    pub latest_value: Cell<Option<NodeId>>,
    /// Companion → owner backlink (`_parentSource`).
    pub parent_source: Cell<Option<NodeId>>,

    // ------------------------------------------------------------ effect half
    /// `None` means "not an effect" — upstream's absent `_type`.
    pub effect_kind: Cell<Option<EffectKind>>,
    pub modified: Cell<bool>,
    pub prev_value: RefCell<Option<AnyValue>>,
    pub effect_fn: RefCell<Option<Rc<RefCell<EffectFn>>>>,
    pub error_fn: RefCell<Option<Rc<RefCell<ErrorFn>>>>,
    /// Cached runner, so re-enqueueing an effect does not allocate. Port of
    /// `_boundRunEffect`.
    pub bound_run_effect: RefCell<Option<EffectRunner>>,
}

impl Node {
    /// Field-for-field zero state. Every constructor starts here and then sets
    /// only what its upstream object literal sets, so a field added upstream
    /// shows up as a compile-time hole rather than a silent default.
    pub(crate) fn blank(kind: NodeKind, queue: QueueId, context: ContextRecord) -> Node {
        Node {
            kind,
            id: RefCell::new(None),
            #[cfg(feature = "dev")]
            name: Cell::new(""),
            config: Cell::new(0),

            value: RefCell::new(None),
            pending_value: RefCell::new(None),
            equals: RefCell::new(None),
            unobserved: RefCell::new(None),
            subs: Cell::new(None),
            subs_tail: Cell::new(None),
            time: Cell::new(0),
            transition: Cell::new(None),

            parent: Cell::new(None),
            first_child: Cell::new(None),
            next_sibling: Cell::new(None),
            prev_sibling: Cell::new(None),
            pending_first_child: Cell::new(None),
            disposal: RefCell::new(Vec::new()),
            pending_disposal: RefCell::new(Vec::new()),
            cleanup: RefCell::new(None),
            context: RefCell::new(context),
            child_count: Cell::new(0),
            queue: Cell::new(queue),
            root_parent_computed: Cell::new(None),

            compute: RefCell::new(None),
            deps: Cell::new(None),
            deps_tail: Cell::new(None),
            dep_gen: Cell::new(0),
            flags: Cell::new(0),
            status_flags: Cell::new(0),
            height: Cell::new(0),
            next_heap: Cell::new(None),
            prev_heap: Cell::new(NodeId::default()),
            error: RefCell::new(None),
            blocked: Cell::new(false),
            pending_sources: RefCell::new(None),
            reask: Cell::new(false),
            loading: Cell::new(false),
            notify_status: RefCell::new(None),
            child: Cell::new(None),
            next_child: Cell::new(None),
            firewall: Cell::new(None),
            in_flight: Cell::new(0),
            in_flight_stream: RefCell::new(None),
            pending_signal: Cell::new(None),
            latest_value: Cell::new(None),
            parent_source: Cell::new(None),

            effect_kind: Cell::new(None),
            modified: Cell::new(false),
            prev_value: RefCell::new(None),
            effect_fn: RefCell::new(None),
            error_fn: RefCell::new(None),
            bound_run_effect: RefCell::new(None),
        }
    }

    /// Port of the `(el as any)._fn` probe.
    #[inline]
    pub fn is_computed(&self) -> bool {
        self.compute.borrow().is_some()
    }

    #[inline]
    pub fn is_root(&self) -> bool {
        matches!(self.kind, NodeKind::Root)
    }

    #[inline]
    pub fn has_flag(&self, mask: u32) -> bool {
        self.flags.get() & mask != 0
    }

    #[inline]
    pub fn has_status(&self, mask: u32) -> bool {
        self.status_flags.get() & mask != 0
    }

    #[inline]
    pub fn has_config(&self, mask: u32) -> bool {
        self.config.get() & mask != 0
    }

    #[inline]
    pub fn set_flags_or(&self, mask: u32) {
        self.flags.set(self.flags.get() | mask);
    }

    #[inline]
    pub fn clear_flags(&self, mask: u32) {
        self.flags.set(self.flags.get() & !mask);
    }

    /// The value a reader sees when it is *not* reading through a transition
    /// hold — the staged value if a write is held, else the committed one.
    pub(crate) fn staged_or_committed(&self) -> Option<AnyValue> {
        let staged = self.pending_value.borrow().clone();
        staged.or_else(|| self.value.borrow().clone())
    }
}
