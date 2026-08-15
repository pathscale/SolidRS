//! The thread-local runtime: the node and link arenas, the module-level
//! mutable state that upstream keeps as `export let` bindings, and the
//! arena-access discipline the rest of the crate relies on.
//!
//! # Two arenas, deliberately
//!
//! Nodes and links live in **separate** `RefCell`s. That is not incidental:
//! `link()` runs inside a read and must allocate an edge while node fields are
//! being read, and the dispose/trim walks must free edges while iterating
//! them. One combined cell would make both a double-borrow panic. Split, the
//! rule falls out naturally:
//!
//! * node fields are reached through `nodes.borrow()` — shared, always, because
//!   every mutable field of [`Node`] is behind its own cell;
//! * `nodes.borrow_mut()` is for allocation and reclamation only;
//! * link *topology* changes take `links.borrow_mut()`; link traversal that
//!   cannot free takes `links.borrow()`.
//!
//! # The one rule
//!
//! A borrow may span engine-internal work — including recursive walks that
//! mutate one node while iterating another's list — but must be dropped before
//! either of two things happens:
//!
//! 1. user code runs (it may create nodes), or
//! 2. a node is allocated or reclaimed.
//!
//! Every function that runs user code — `recompute`, `run_effect`,
//! `run_disposal` — is written as *borrowed prologue → drop → user call →
//! borrowed epilogue*, mirroring the phases the TypeScript already has.
//!
//! # Why the arena is not `Rc<RefCell<Node>>`
//!
//! A reactive graph is cyclic by construction: a dependency points at its
//! subscribers and every subscriber points back at its dependencies.
//! Refcounting that never reaches zero. Generational keys make the cycles
//! harmless — a stale key resolves to `None`, not to freed memory — and let
//! disposal be an explicit, auditable step rather than an emergent property of
//! drop order.

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::collections::VecDeque;
use std::rc::Rc;

use slotmap::SlotMap;

use crate::constants::reactive;
use crate::heap::Heap;
use crate::node::{ContextRecord, Link, LinkId, Node, NodeId, QueueId, TransitionId};
use crate::scheduler::{Queue, Transition};

pub(crate) type Nodes = SlotMap<NodeId, Node>;
pub(crate) type Links = SlotMap<LinkId, Link>;

/// Resolve a node key the engine reached from a live edge.
///
/// Panics only on an internal bug: reclamation unlinks first, so anything
/// still addressable from an edge must resolve.
#[inline]
#[track_caller]
pub(crate) fn node(nodes: &Nodes, id: NodeId) -> &Node {
    nodes
        .get(id)
        .expect("solid_rs: node reached from a live edge was already reclaimed")
}

#[inline]
#[track_caller]
pub(crate) fn link_of(links: &Links, id: LinkId) -> &Link {
    links
        .get(id)
        .expect("solid_rs: link reached from a live edge was already reclaimed")
}

/// Everything upstream keeps as module-level `let` in `core.ts` and
/// `scheduler.ts`.
///
/// Scalars are `Cell`, not `RefCell`: `read()` touches `ctx`, `tracking` and
/// `clock` on every call, and a borrow flag on that path is pure overhead for
/// state that is never borrowed across a call.
pub(crate) struct Runtime {
    nodes: RefCell<Nodes>,
    links: RefCell<Links>,

    // -------------------------------------------------------- core.ts globals
    /// `context` upstream: the current owner.
    pub ctx: Cell<Option<NodeId>>,
    pub tracking: Cell<bool>,
    /// `stale` upstream: render and pure effects read committed values while a
    /// transition holds newer ones.
    pub stale: Cell<bool>,
    /// Set while an `isPending()` probe is collecting. Never true in the
    /// synchronous core (the verdict layer is out of scope); the branches that
    /// test it are ported so that layer drops in without reshaping `read`.
    pub pending_check_active: Cell<bool>,
    /// Set while a `latest()` read window is open. Same status as above.
    #[allow(dead_code, reason = "read by verdict.ts, a separate port")]
    pub latest_read_active: Cell<bool>,
    #[allow(dead_code, reason = "read by the snapshot layer, a separate port")]
    pub snapshot_capture_active: Cell<bool>,
    /// Component-body read guard. Set by `untrack(fn, label)`, which is part of
    /// the renderer integration rather than the core.
    #[cfg(feature = "dev")]
    #[allow(dead_code, reason = "set by the renderer integration, not by the core")]
    pub strict_read: Cell<Option<&'static str>>,

    // --------------------------------------------------- scheduler.ts globals
    pub clock: Cell<u64>,
    pub active_transition: Cell<Option<TransitionId>>,
    pub scheduled: Cell<bool>,
    pub halted: Cell<bool>,
    pub halt_notified: Cell<bool>,
    pub sync_depth: Cell<u32>,
    pub projection_write_active: Cell<bool>,
    /// Sticky: flips true on the first `refresh()` ever, so the hot
    /// notification loop skips the per-subscriber re-ask clear entirely in
    /// graphs that never refresh.
    pub reask_armed: Cell<bool>,
    /// Monotonic pass token for `Queue::run`'s idempotent child traversal.
    pub queue_run_token: Cell<u64>,

    // ------------------------------------------------------------------ heaps
    pub dirty_queue: Heap,
    pub zombie_queue: Heap,

    // ----------------------------------------------------------------- queues
    pub queues: RefCell<SlotMap<QueueId, Queue>>,
    pub global_queue: Cell<QueueId>,
    /// `globalQueue._running`.
    pub queue_running: Cell<bool>,

    // ------------------------------------------------------------ transitions
    pub transitions: RefCell<SlotMap<TransitionId, Transition>>,
    /// The live transition set (`const transitions = new Set<Transition>()`).
    pub live_transitions: RefCell<Vec<TransitionId>>,
    /// `globalQueue._batch` — the current transaction-shaped batch. Ambient
    /// work is a transaction too, so this is never absent.
    pub batch: Cell<TransitionId>,

    // -------------------------------------------------------------- microtask
    /// See [`crate::host`]. There is no JS event loop here, so the microtask
    /// queue is ours to provide and ours to drain.
    pub microtasks: RefCell<VecDeque<Box<dyn FnOnce()>>>,

    /// Shared empty context record (`defaultContext` upstream).
    pub default_context: ContextRecord,
}

thread_local! {
    pub(crate) static RT: Runtime = Runtime::new();
}

/// Run `f` with the reactive runtime for this thread.
#[inline]
pub(crate) fn with_rt<R>(f: impl FnOnce(&Runtime) -> R) -> R {
    RT.with(f)
}

impl Runtime {
    fn new() -> Runtime {
        let mut queues: SlotMap<QueueId, Queue> = SlotMap::with_key();
        let global = queues.insert(Queue::new_global());

        let mut transitions: SlotMap<TransitionId, Transition> = SlotMap::with_key();
        let batch = transitions.insert(Transition::new(0));

        Runtime {
            nodes: RefCell::new(SlotMap::with_key()),
            links: RefCell::new(SlotMap::with_key()),

            ctx: Cell::new(None),
            tracking: Cell::new(false),
            stale: Cell::new(false),
            pending_check_active: Cell::new(false),
            latest_read_active: Cell::new(false),
            snapshot_capture_active: Cell::new(false),
            #[cfg(feature = "dev")]
            strict_read: Cell::new(None),

            clock: Cell::new(0),
            active_transition: Cell::new(None),
            scheduled: Cell::new(false),
            halted: Cell::new(false),
            halt_notified: Cell::new(false),
            sync_depth: Cell::new(0),
            projection_write_active: Cell::new(false),
            reask_armed: Cell::new(false),
            queue_run_token: Cell::new(0),

            dirty_queue: Heap::new(),
            zombie_queue: Heap::new(),

            queues: RefCell::new(queues),
            global_queue: Cell::new(global),
            queue_running: Cell::new(false),

            transitions: RefCell::new(transitions),
            live_transitions: RefCell::new(Vec::new()),
            batch: Cell::new(batch),

            microtasks: RefCell::new(VecDeque::new()),

            default_context: Rc::new(Default::default()),
        }
    }

    #[inline]
    pub fn nodes(&self) -> Ref<'_, Nodes> {
        self.nodes.borrow()
    }

    #[inline]
    pub fn links(&self) -> Ref<'_, Links> {
        self.links.borrow()
    }

    #[inline]
    pub fn links_mut(&self) -> RefMut<'_, Links> {
        self.links.borrow_mut()
    }

    /// The heap a node belongs to, picked from its own zombie flag.
    ///
    /// Port of `queueFor`. Every `delete_from_heap` call site picks the heap
    /// this way, so a node left physically linked in the wrong heap after
    /// being zombified would be unlinked from the wrong bucket on dispose,
    /// corrupting it and livelocking the next drain (upstream #2759).
    #[inline]
    pub fn heap_for(&self, node: &Node) -> &Heap {
        if node.has_flag(reactive::ZOMBIE) {
            &self.zombie_queue
        } else {
            &self.dirty_queue
        }
    }

    // ------------------------------------------------------------- allocation

    /// Allocate a node. Must not be called while a node-arena borrow is held.
    pub fn alloc_node(&self, node: Node) -> NodeId {
        let id = self.nodes.borrow_mut().insert(node);
        // Upstream initialises `_prevHeap` to the node itself; the key only
        // exists after insertion.
        self.nodes.borrow()[id].prev_heap.set(id);
        id
    }

    // ---------------------------------------------------------- no reclamation
    //
    // Upstream leaves unreachable nodes to the JS garbage collector. This port
    // deliberately has **no** automatic reclamation, and that is a considered
    // decision rather than an omission:
    //
    // * `DISPOSED` is not proof of unreachability. `graph::unobserved` sets it
    //   on an auto-disposing memo that upstream fully expects to resurrect on
    //   the next read (`prepare_computed` recomputes a disposed node). Freeing
    //   on that flag would turn a legal read into a miss.
    // * Public handles are `Copy`, so the runtime cannot know whether user
    //   code can still name a node. Without that, no liveness test is sound.
    //
    // The arena still buys what the port brief asked for — a cyclic graph
    // addressed by generational keys instead of refcounted pointers, where a
    // stale key is a miss rather than a dangling read. Bounding the arena over
    // a long-running app needs handle liveness; see README.md ("Deviations")
    // for the two designs that would provide it.

    /// Number of live nodes. Diagnostic hook, used by the reclamation test.
    pub fn node_count(&self) -> usize {
        self.nodes.borrow().len()
    }

    /// Number of live dependency edges. Diagnostic hook.
    #[allow(dead_code, reason = "surfaced through the public `debug` module")]
    pub fn link_count(&self) -> usize {
        self.links.borrow().len()
    }
}
