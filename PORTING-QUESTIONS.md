# Porting questions and judgement calls

Every place the TypeScript was ambiguous, or where a decision had to be made
that the source does not settle on its own. Nothing here was invented silently:
where a reading had to be chosen, the entry says which one and why.

Upstream pin: `15975306e524f197e2231ba6bd2c259c0dc39362`
(`@solidjs/signals@2.0.0-rc.0`), `packages/solid-signals`.

Sections:

- [A. Brief vs. source discrepancies](#a-brief-vs-source-discrepancies)
- [B. Shape changes forced by Rust](#b-shape-changes-forced-by-rust)
- [C. Semantics chosen where the TypeScript is ambiguous](#c-semantics-chosen-where-the-typescript-is-ambiguous)
- [D. Machinery ported but inert](#d-machinery-ported-but-inert)
- [E. Test-porting decisions](#e-test-porting-decisions)
- [F. Open questions for upstream](#f-open-questions-for-upstream)

---

## A. Brief vs. source discrepancies

### A1. `batch` does not exist in this version

The brief lists `batch` in the synchronous surface to port. It is not exported
from `src/index.ts`, not defined anywhere in `packages/solid-signals/src`, and
has no test.

**Why it's gone, not missing:** batching is structural in 2.0. Writes stage into
`_pendingValue` (`setSignal`, `core.ts:1029`) and commit at flush
(`commitPendingNodes`, `scheduler.ts:709`), so several writes in a tick already
collapse into one update pass with no wrapper. `flush(fn)` is the closest thing
to an explicit scope, and it is ported as [`flush_with`].

**Chosen:** not ported; [`flush_with`] covers the "run these writes and drain"
shape. `tests/flush.rs::should_batch_updates` pins that consecutive writes
coalesce without any batching call.

### A2. `useContext` does not exist in this version

The brief lists `createContext`/`useContext`. `context.ts` exports
`createContext`, `getContext` and `setContext`; its own doc comment says the
user-facing `useContext` "in `solid-js`" wraps `getContext` — i.e. it lives in
the renderer package, not here.

**Chosen:** ported `create_context` / `get_context` / `set_context`. No
`use_context`.

### A3. "error boundaries' sync paths" spans a file the brief excludes

`createErrorBoundary` lives in `src/boundaries.ts`, which the brief puts out of
scope, but "error boundaries' sync paths" is listed as in scope.

**Chosen reading:** the *engine-side* error machinery is in scope, the boundary
component is not. So this port includes `notifyStatus`/`clearStatus` (from
`async.ts`, in `src/status.rs`), the `StatusError` wrapper and source tracking,
identity-based error propagation, the silent-recovery sweep
(`settleErroredDependents`), the effect error arm, the `Queue.notify` chain, and
`haltReactivity`. It does not include `createErrorBoundary` itself. The
`notify_hook` slot on `Queue` is the seam a boundary port installs into.

Three tests are `#[ignore]`d for this reason and would pass once boundaries land.

### A4. `tests/core/**` contains no tests

The brief lists `tests/core/**` as required. The directory holds exactly one
file, `reactivity.bench.ts` — a Vitest benchmark, not a test suite.

**Chosen:** nothing to port. The benchmark shapes (deep propagation, wide fan-out,
diamond) are already covered by `tests/graph.rs`.

### A5. Four `async.ts` functions are reachable from synchronous code

`async.ts` is out of scope, but `recompute` calls into it on paths a purely
synchronous graph reaches: `clearStatus`, `notifyStatus`,
`settleErroredDependents`, and the `setPendingError`/`addPendingSource`
helpers those need.

**Chosen:** ported into `src/status.rs` rather than stubbed, because without
them errors do not propagate at all. `handleAsync`, `settlePendingSource`,
`parkLoadingWindow` and `releaseSettledDependents` stay with the async port
(the last is ported but unused, since only the async path strands lazy nodes).

---

## B. Shape changes forced by Rust

### B1. Errors are returned, not thrown

Upstream throws out of `_fn` and catches in `recompute`; a read of an errored
node rethrows.

Rust could do this with `catch_unwind`, but a panic is the wrong carrier: the
graph must **store** the error on a node, propagate it **by object identity**
(`sub._error !== error`, `node._error === error`), and re-deliver it to late
readers. A panic payload supports none of that without immediately boxing it
into exactly the value this port uses anyway.

**Chosen:** a computation is `FnMut(Option<&T>) -> Result<T, ReactiveError>`;
`ReactiveError` holds its payload behind an `Rc` and compares with
`Rc::ptr_eq`, preserving the identity semantics the propagation and
silent-recovery sweeps depend on. `recompute` routes `Err` through the same
`notify_status(STATUS_ERROR, …)` call upstream's `catch` does. Nothing about
*which node owns an error, how it propagates, or when it recovers* changed.

The ergonomic constructors (`create_memo`, `create_effect`) wrap into `Ok`; the
fallible ones (`try_create_memo_with`, `try_create_effect`) expose the channel.

**One place a panic remains:** an error that escapes every boundary. Upstream
calls `haltReactivity` and then *rethrows*, unwinding the flush into the
caller. The Rust equivalent is a panic out of `flush()`, and the runtime is
left usable — `global_flush` restores `queue_running` from a `Drop` guard, and
`flush_with` restores `sync_depth` the same way. Without those guards the
runtime wedges after the very halt it was reporting; `tests/flush.rs` pins it.

### B2. Values are `Rc<dyn Any>`, not `Box<dyn Any>`

Two independent reasons, both from the port rather than from taste:

1. `runEffect`'s `finally` does `node._prevValue = node._value` — the *same* JS
   object in two slots. `Box` cannot express that.
2. Every path that hands a value to user code must release the arena borrow
   first (see B4). A refcount bump makes that a one-liner; a `Box` would need a
   take/restore dance around every call, with a hole if the callee re-enters.

This is not refcounting the graph. Values are acyclic payloads; edges remain
generational keys.

### B3. One `Node` type instead of three overlapping shapes

Upstream has `RawSignal`, `Owner`, and `Computed extends RawSignal, Owner`,
discriminated structurally at runtime (`(el as any)._fn`, `(el as Root)._root`,
`(el as any)._type`). Rust has no structural typing, and three arenas would put
a tag on every edge.

**Chosen:** one `Node` with a `NodeKind` discriminant and the union of fields.
The structural probes become `node.is_computed()`, `node.is_root()`,
`node.effect_kind.get()`. `node.rs` carries the mapping table.

### B4. Interior mutability per field, and the one borrow rule

The graph algorithms mutate one node while iterating another's link list, which
`&mut` aliasing forbids outright. So each mutable field carries its own
`Cell`/`RefCell` and the arena is only ever borrowed shared.

Nodes and links live in **separate** `RefCell`s: `link()` allocates an edge
while node fields are being read, and the dispose/trim walks free edges while
iterating them — one combined cell makes both a double-borrow panic.

The invariant, stated in `runtime.rs`: *a borrow may span engine-internal work,
but must be dropped before user code runs or a node is allocated.* Functions
that can reach user code (`unlink_subs`, `dispose_children`, `recompute`,
`run_effect`, `read_node`) take short borrows internally rather than receiving
them, so nothing is deferred to a later "safe point" — deferring would reorder
cleanups, which `tests/on_cleanup.rs` pins exactly.

### B5. Handles are `Copy`; arena slots are never reclaimed

See README "Deviations". Automatic reclamation was implemented and then
**removed** as unsound: `graph::unobserved` sets `DISPOSED` on an auto-disposing
memo that upstream fully expects to resurrect on the next read
(`prepareComputed` recomputes a disposed node), so `DISPOSED` is not proof of
unreachability. With `Copy` handles the runtime cannot know whether user code
can still name a node, so no liveness test is sound.

**Chosen:** no reclamation, documented, with the two designs that would fix it
sketched in the README. A wrong collector is worse than none.

### B6. `equals` defaults require `PartialEq`

Upstream's default comparator is `isEqual = (a, b) => a === b`. Rust's
equivalent for a typed value is `PartialEq`.

**Chosen:** `create_signal`/`create_memo` require `T: PartialEq`;
`create_signal_with`/`create_memo_with` take `Options<T>`, whose constructors
are `Options::equals(f)`, `Options::never_equal()` (upstream's `equals: false`)
and `Options::default()` (`PartialEq`, requires the bound). There is
deliberately **no** `Options::new()`: a constructor that silently picked a
non-default comparator for non-`PartialEq` types would be a trap.

### B7. `setSignal` vs `setMemo` dispatch moved to the node

Upstream returns a different setter for `createSignal(value)` (`setSignal`) and
`createSignal(fn)` (`setMemo` = write **plus** suppress the queued recompute).

**Chosen:** one `WriteSignal<T>` type; the branch is taken on the node
(`is_computed()`), because only a writable memo is a computed. This cannot be
misapplied and keeps both constructors returning the same setter type. The
`#2692`/`#2745` "manual write wins" tests pin it — and caught the bug when the
suppression was missing.

### B8. Cleanup return values are statically typed

The apply phase returns `impl IntoCleanup` (`()` or `Option<Disposable>`), so
upstream's runtime "callback returned an invalid cleanup value" error is a
compile error. Two tests are `#[ignore]`d as not representable.

### B9. `refresh()` is a method, not a branded free function

Upstream's `refresh(target)` takes any value and validates the `$REFRESH` brand
at runtime, throwing `INVALID_REFRESH_TARGET` for wrappers, plain objects and
property reads. Here `refresh()` is an inherent method on `ReadSignal`/`Memo`,
so those three failure modes are compile errors. Three tests `#[ignore]`d.
Refreshing a plain signal remains a runtime no-op, as upstream — that one is
ported and passes.

### B10. Context is typed; defaults are `fn() -> T`

Upstream keys contexts with `Symbol()` and reads them untyped. Here
`Context<T>` carries the value type (a mismatched read is a compile error) and
identity comes from a thread-local counter.

The default value is stored as `fn() -> T` rather than a `T`, which keeps
`Context<T>` `Copy` — so it drops into closures without ceremony — without
forcing the default to be `&'static`.

`get_context`/`set_context` return `Result` instead of throwing
`NoOwnerError`/`ContextNotFoundError`; the error types are ported and public.

---

## C. Semantics chosen where the TypeScript is ambiguous

### C1. `_error = null` vs `_error = undefined`

`setPendingError(el)` with no source assigns `el._error = null`, while an
untouched node has `undefined`. Both are falsy, and every read site tests
truthiness (`if (!el._error)`) — except `recompute`'s
`outgoingError !== undefined`, which distinguishes them.

**Analysis:** `outgoingError` is only read under `el._statusFlags & STATUS_ERROR`,
where `_error` is always a real error object, never `null`.

**Chosen:** collapsed both to `Option::None`. Behaviour is identical on every
reachable path. Flagged here because the distinction is load-bearing *in
principle* and the async port should re-check it when `parkLoadingWindow` (which
does set `null` under `STATUS_ERROR`-adjacent states) lands.

### C2. `recompute` reads the previous value across the computation

`recompute` computes `value = el._pendingValue === NOT_PENDING ? el._value :
el._pendingValue` and then calls `el._fn(value)`. Upstream never re-reads
`_value` between that read and the epilogue's assignment — but a nested read of
the node *from inside its own computation* would see the old value in JS.

**Chosen:** the previous value is cloned out (an `Rc` bump) before the call, so a
self-read inside the computation sees the same thing JS would. No behavioural
difference; noted because the earlier take-and-restore approach did have one.

### C3. Effect `prev_value` after a failed apply phase

Upstream's `finally` sets `node._prevValue = node._value` whether the apply
phase threw or not. **Chosen:** same — `prev_value` rolls forward on both arms,
so a failing effect does not repeat the same `prev` on its next run.

### C4. `staleValues` for render effects

`recompute` sets `stale = true` for any effect that is not `EFFECT_USER`, so
render and pure effects read committed values while a transition holds newer
ones. With no transitions reachable synchronously this is unobservable, but it
is ported as written rather than dropped, because `read()`'s value selection
branches on it.

### C5. `queue_notify` returns `false` for `STATUS_ERROR` at the root

`GlobalQueue.notify` returns `true` only for `mask & STATUS_PENDING`; an error
notification that reaches the root returns `false`, which is what makes
`notifyEffectStatus` halt and rethrow. This is the sync error path and is ported
exactly. It is also why a render-effect compute error with no boundary panics.

### C6. `is_empty_batch` omits `transient_store_nodes`

`canUseSimpleSyncFlush` also checks `transientStoreNodes.size === 0`. That set
is populated only by the proxy store (`registerTransientStoreNode`), a separate
port.

**Chosen:** the check is omitted rather than stubbed with a permanently-empty
set. The store port must re-add it; noted here so it is not rediscovered.

---

## D. Machinery ported but inert

Per constraint 4, the queue model, notification model and execution-control
hooks are ported as they are, even where a synchronous caller never reaches
them. These are live code with no live producer:

| Ported | Producer (out of scope) |
|---|---|
| `Transition` (all fields), `live_transitions`, `current_transition`, `run_in_transition` | `async.ts`, `action.ts` |
| `QueueStub` stash/restore shape | the incomplete-transition path in `async.ts` |
| `Queue::notify_hook`, `check_sources`, the queue tree | `boundaries.ts` |
| async-reporter registration in `queue_notify` | needs a `NotReadyError` to register |
| `Node::child` / `next_child` / `firewall` and the walks over them | `store/` |
| `Link::pending_observer` and its branches in `link`/`notify_status` | `verdict.ts` |
| `pending_check_active`, `latest_read_active` and their `read()` branches | `verdict.ts` |
| `REACTIVE_OPTIMISTIC_DIRTY`, `Node::transition`, lane routing point in `queue_enqueue` | `lanes.ts`, `optimistic.ts` |
| `Node::reask` / `REACTIVE_REASK` / `arm_reask_clear` | `verdict.ts` classifies it; `refresh()` **does** set it here |
| `Node::loading` / `Options::loading_value` | the value is honoured; the window only matters with async |
| `status::PENDING` branches in `read`, `unlink_subs`, `notify_status` | `async.ts` |

`finalize_pure_queue` has a comment block listing, in call order, the five
upstream steps whose gates are empty here (optimistic resolution, `affects()`
mark release, optimistic store clear, transient store sweep, lane cleanup) so
the later ports slot back in at the right points.

Two upstream call sites are noted in code rather than ported:

- `disposeChildren`'s `_snapCompanions` call (INV-9: a disposed source must not
  leave an `isPending` verdict latched true) — `owner.rs`.
- `recompute`'s `_syncCompanions` call on a transition-held landing — `core.rs`.

Both belong to the verdict layer.

### Invariants

`invariants.ts` is ported behind the `test_invariants` feature (upstream's
`__TEST__`, deliberately separate from `__DEV__` — they measured 5-21% on their
bench suite). INV-1 and INV-7 are live and load-bearing. INV-2/3/4/5/6/9/10 are
documented in a table in `src/invariants.rs` with the port that re-arms each;
they are inert because their subject state cannot exist yet, not because they
were skipped.

### Import-cycle hooks

Upstream breaks module cycles with static assignment (`GlobalQueue._update =
recompute`, `._dispose`, `._runEffect`). Rust allows mutual module references
inside a crate, so those three are direct calls. The *pluggable engine* hooks —
the ones that are an extension seam rather than a cycle workaround — are kept
as hook slots (`Queue::notify_hook`, `Node::notify_status`).

---

## E. Test-porting decisions

Ported tests keep their upstream name in a doc comment above the Rust function,
so a failure maps back to the TypeScript by grep. 132 pass, 19 are `#[ignore]`d
with a one-line reason.

### E1. Upstream tests that rely on eager memo creation

Several `graph.test.ts` tests never read the tail memo — they rely on memos
computing eagerly at creation. An added `.get()` would trip the ownerless
auto-dispose tail in `read()` and break the test. The ports do not add reads.
(This bit during porting and is worth knowing: `let _ = memo;` not `memo.get();`.)

### E2. `ensures that new dependencies are updated before dependee`

Upstream's `c` references `e`, which is declared *after* it — JS `var` hoisting.
Creation order is exactly what the test measures, so the port preserves it with
a forward slot (`Rc<Cell<Option<Memo<i32>>>>`) rather than reordering the
declarations, which would change the expected string from `"bcd"` to `"bdc"`.

### E3. `should not observe`

Upstream asserts the *structural absence* of `_subs`/`_deps` on a `Root`
object. Every node here has those fields. **Chosen:** assert the equivalent
invariant — a root neither subscribes nor is subscribed to — via the `debug`
module.

### E4. `should scope function to current scope`

Upstream pokes `owner._context = { foo: 1 }` directly. Context is typed here, so
the port writes through `set_context`. Same invariant: the captured owner's
context is visible when re-entered.

### E5. `balances syncDepth when an effect throws inside flush(fn)`

Upstream's effect throws; here the apply phase returns `Err`, which is the same
escalation channel (B1). The invariant under test — a failing effect inside
`flush(fn)` must not leak the sync-depth counter — is unchanged, and the port
found a real bug: the panic also had to not leak `queue_running`.

### E6. `createMemo.test.ts` ignore granularity

That file is ~2 700 lines, the large majority async. Rather than 40+ near-identical
`#[ignore]` stubs, the async blocks are represented by one marker per upstream
`describe` (`describe_async_compute`, `describe_is_pending_and_latest`), with
individually-named markers kept for tests that are *nearly* in scope (the
`refresh()` group). Every synchronous test in the file is ported individually.

### E7. Tests added beyond the upstream set

Four, each pinning something this port has to provide that upstream gets for
free, and each labelled "Not upstream" in place:

- `flush.rs::writes_settle_on_the_microtask_checkpoint_without_an_explicit_flush`
  — the microtask seam (constraint 5).
- `autodispose_contract.rs::lazy_memo_releases_on_last_subscriber_and_recomputes_for_the_next`
  and `owned_eager_memo_is_not_released_when_it_loses_subscribers` — the
  synchronous half of the `AUTO_DISPOSE` gate, since all four upstream tests in
  that file are async.
- `context.rs::owned_context_values_round_trip` — the Rust API distinguishes
  owned from `'static` values (B10), so the owned path needs its own pin.

---

## F. Open questions for upstream

Recorded, not acted on — per the brief, no later commits were consulted.

1. **`heap.ts:71` — `insertIntoHeap` early-out ordering.** The function returns
   early on `REACTIVE_MANUAL_WRITE`, but `insertIntoHeapHeight` also early-outs
   on it, so a manually-written node whose *height* changed never gets its
   height adjusted until the flag clears at commit. Reachable? The
   `#2692` tests pass either way, so the port follows the source exactly.

2. **`core.ts:340` — `(!isEffect && wasUninitialized) || !el._equals || …`.**
   The uninitialized short-circuit is gated on `!isEffect`, but effects always
   have `_equals: false`, so the second clause already forces `true` for them.
   The `!isEffect` guard looks redundant. Ported verbatim in case an effect can
   acquire a comparator through a path this port does not cover.

3. **`graph.ts:109` — `_pendingObserver` labelling outside a recompute.** The
   comment says "outside a recompute there is no pass boundary, so the latest
   read labels it", which makes the label order-dependent for untracked probe
   reads. Intentional, or an artefact of there being no generation to compare?
   Inert here (no probes), so the source is followed exactly.

4. **`scheduler.ts:492` — `scheduled` assignment in the incomplete-transition
   branch** reads `this._batch._pendingNodes.length > 0` *after* the batch may
   have been replaced two lines earlier. The two orders differ when the batch
   was replaced and the fresh one is empty. Unreachable synchronously; the async
   port should confirm which order is intended.

---

## G. Phase 2 (async layer) — judgement calls

Recorded during the async foundation. See CHANGELOG-PHASE2.md for scope.

### G1. `Promise` has no Rust equivalent, and does not need one

Upstream's async layer rests on `PromiseLike` (whose `.then` may fire
*synchronously* — `handleAsync`'s `isSync` latches depend on it),
`AsyncIterable`, and the event loop draining microtasks.

**Analysis:** upstream never *polls* a future. It registers a continuation and
the platform calls it. A runtime would contribute only the call, and phase 1
already ported the queue that makes calls.

**Chosen:** `Flight<T>` (one-shot) and `Streaming<T>` (sequence), settled from
outside, posting their continuation onto the microtask queue. `Async<T>` names
the three shapes upstream discovers by probing the returned object. No tokio,
no async-std, nothing beside the scheduler — constraint 2 held without strain.

Consequence worth noting: upstream's `SYNC_NODE_RECEIVED_ASYNC` diagnostic (a
`sync: true` node returning a Promise) becomes unrepresentable — `Options::sync`
and `create_async_memo` are different constructors. One fewer runtime error.

### G2. Where the spec and the implementation disagreed

Only one place surfaced at foundation level, and the implementation won per
constraint 4:

`SPEC-ASYNC-SEMANTICS.md` A19 says an uninitialized source's `NotReadyError`
"must propagate to loading boundaries", implying the read always raises. The
implementation's `read()` raises only when there is a reading context *or* the
node is `STATUS_UNINITIALIZED` — an *initialized* node that is pending with no
observer returns its stale value instead. Ported the implementation's condition.
Pinned by `async_memo::should_return_stale_value_when_reading_re_pending_async_computed`.

### G3. `_inFlight` is cleared only by `recompute`

Upstream's `asyncWrite` and `handleError` never clear `_inFlight`; only
`recompute`'s prologue does, when it replaces the flight. This is not incidental
— a stream keeps landing through that identity, so clearing it on the first
landing closes every stream after one yield. Found the hard way; the guard is
now commented in `asyncrt.rs`.

### G4. Two phase-1 simplifications this layer had to undo

Both were places phase 1 *removed* something for being unreachable, rather than
modelling something wrongly. Detail in CHANGELOG-PHASE2.md; summarised:

1. `read()`'s **suspension link** — the `if (!tracking && el !== c) link(el, c)`
   in each pending arm. Without it an untracked read of a pending node records
   no edge, so the settle sweep cannot reach the reader and it never wakes.
2. `global_flush`'s **transition branch**, stubbed in phase 1 with a comment
   calling it unreachable. It is the whole incomplete-transition protocol.

Everything else phase 1 kept as inert scaffolding was shaped correctly and
needed no rework — which is the useful signal about how clean the sync/async
split was.

---

## H. `map.ts` — `map_array` and `repeat`

**An upstream oracle exists**, and it is the primary source for this port:
`tests/mapArray.test.ts` (313 lines, 7 tests) and `tests/repeat.test.ts`
(421 lines, 11 tests). All 18 are ported, keeping their upstream names. Six
further tests are added for coverage the upstream file does not carry; each is
labelled "Not upstream" in place, so the two kinds are never confused.

Two sibling upstream files are **not** ported and are named here rather than
silently skipped:

- `tests/mapArray-notready-safety.test.ts` (579 lines) — the `NotReadyError`
  mid-pass abort/retry contract (#2903). Needs a fallible mapper *and* loading
  boundaries; see H4.
- `tests/maparray-store-nonkeyed.test.ts` (75 lines) — store proxies, a
  separate port.

### H1. Four overloads become three named functions

Upstream picks behaviour from `options.keyed` **and** from `map.length` — JS
function arity (`map.ts:73`, `const indexes = map.length > 1`). Rust closures
have no introspectable arity.

**Chosen:** three functions, one per keying mode — `map_array` (keyed by
identity), `map_array_by_index` (`keyed: false`), `map_array_keyed`
(`keyed: item => key`) — each taking the exact callback shape its overload
specifies.

**Consequence, deliberate:** the arity check is upstream's way of *not
allocating index signals* when the callback ignores the index. Here the
index-taking forms always allocate one. This is a per-row allocation, not a
semantic difference: a callback that ignores its index observes nothing
different. Recorded rather than worked around, because a future
`map_array_values` (item only, no index) would close it.

### H2. The mapped array is `Rc<Vec<M>>`, compared by pointer

Upstream returns the **same JS array** when a pass produced no structural
change (`map.ts:234-237`) and a fresh one otherwise — with the comment "new
identity for downstream change propagation" (`map.ts:287`). Array identity is
load-bearing: it is what stops downstream memos re-running.

**Chosen:** the memo's value is `Rc<Vec<M>>` and its comparator is
`Rc::ptr_eq`. That is JS array identity exactly. Pinned by the added test
`an_unchanged_pass_keeps_the_mapped_array_identity`.

**Bound this forces:** `M: Clone`. Retained rows must be copied from the old
array into the new one, and they cannot be moved out of an `Rc` the consumer
may still hold. A caller whose mapped value is expensive should map to
`Rc<T>`; the clone is then a refcount bump. (`Vec<Rc<M>>` internally would
avoid the bound but leaks `Rc` into every callback signature.)

### H3. `a === b || compare(key, a, b)` is fused into one predicate

Upstream's prefix/suffix scans test
`this._items[i] === newItems[j] || (this._rows && compare(this._key, …))`
(`map.ts:213-214`, `226-227`).

**Analysis:** a key match subsumes reference equality — identical values yield
identical keys — so the disjunction is equivalent to a single predicate chosen
per mode: identity → `a == b`; keyed-by-fn → `key(a) == key(b)`; by-index →
`true` (upstream's `compare(undefined, …)` returns `true`, `map.ts:451`).

**Chosen:** one `same_position` closure built at construction. This is not only
tidier — it avoids demanding `PartialEq` on `Item` in keyed-by-function mode,
where upstream needs no such capability from the item type.

**Row-signal equality is structural, not referential.** Upstream's row signals
use the default `isEqual` (`===`), so writing a *distinct but equal* object
notifies. Rust's `PartialEq` says they are equal and the write is a no-op. The
divergence is observable only through reference identity, which Rust values do
not have; every ported test passes on both readings.

### H4. The staged-commit abort path is unreachable, and recoverable

`updateKeyedMap` and `updateRepeat` stage all work and commit only after every
mapper succeeded, so a `NotReadyError` thrown mid-pass leaves prior state
uncorrupted for the post-settle retry (#2903, `map.ts:108-116`).

**Ported:** the staging structure and — importantly — its *ordering*
consequence: removed rows dispose **after** the pass's new rows are created.
That ordering is observable and is pinned.

**Not reachable:** the abort arm itself. The public mappers are infallible
(`FnMut(..) -> M`), so no pass can fail. Making it reachable needs a
`try_map_array` taking `FnMut(..) -> Result<M, ReactiveError>`, which is only
meaningful once loading boundaries exist. Deliberately not added on
speculation — an untested abort path is worse than a documented absent one.

**But the absence is recoverable, which is the part that matters.** Phase 1's
own finding was that the expensive mistake is deleting machinery for being
unreachable, because re-deriving where it belonged costs more than carrying it.
"Unreachable, see #2903" cannot be rebuilt from, so the three sites are marked
`ABORT SITE 1/2/3` in `src/map.rs`, and the contract block on `update_keyed_map`
records all three things a future implementer would otherwise re-read upstream
to recover:

| | |
|---|---|
| upstream locations | `map.ts:182-188` (create fast path), `map.ts:275-284` (general path step 2), `map.ts:424-435` (`updateRepeat`) |
| reachability condition | a mapper returning `Result<M, ReactiveError>`, plus somewhere for a suspended read to suspend *to* (`boundaries.ts`) |
| observable difference | owners created earlier in the failing pass stay alive and unreferenced — their effects keep running against rows the retry rebuilds, so a suspended list accumulates one leaked scope per row per retry. The arm's only job is that disposal; the state it protects is already safe, because nothing is written to `data` before the commit step |
| the test that would catch it | map a list whose callback suspends on the second row, settle, assert live scope count equals row count rather than twice it |

### H9. The ignored-test taxonomy

The suite carries 15 `#[ignore]`s. Reported here rather than left to a grep,
because the count is cited as a result.

By the label each carries:

| label | count |
|---|---:|
| `blocked on boundaries.ts` | 5 |
| `blocked on lanes.ts` | 2 |
| `phase 2` | 3 |
| `not representable` | 5 |

**That grouping mixes a label with a blocker, and should not be reported as
four peer categories.** Reading the reason text, the three `phase 2` entries
each name a concrete blocker: one needs `lanes.ts`, one needs
`boundaries.ts` + `lanes.ts`, one needs `optimistic.ts` + `lanes.ts`. Grouped
by *actual* blocker (a test may have more than one, so these overlap):

| blocker | count |
|---|---:|
| `boundaries.ts` | 6 |
| `lanes.ts` | 5 |
| `optimistic.ts` | 1 |
| not representable in Rust | 5 |

Only the last is a closed category: those five cannot be written because the
value the upstream test constructs — an invalid cleanup return, a non-source
`refresh` target — is unconstructible in Rust, so the failure mode upstream
tests at runtime is a compile error here. The other three shrink to zero as
their modules land.

**One caveat that matters for any count.** Two of the 15 are `describe`-block
stand-ins, not single tests: `describe_async_compute` represents 40+ upstream
tests and `describe_is_pending_and_latest` a whole block. So "15 ignored"
understates the not-ported upstream surface by roughly an order of magnitude,
and should not be read as "15 upstream tests remain".

### H5. `$TRACK` has no analogue

`updateKeyedMap` reads `(newItems as any)[$TRACK]` for "top level tracking"
(`map.ts:120`) — subscribing to a store proxy's *structure*, since a proxy's
array reads would otherwise not register a dependency on length/order.

**Chosen:** omitted. Here `list` is a plain accessor; reading it tracks
normally. The store port must reintroduce it.

### H6. Three `repeat` tests source data from `createStore`

`should compute keyed map`, `should notify observer`, and `should retain
instances when only offset changes` use `createStore`.

**Chosen:** a signal holding a `Vec`, with the row callbacks reading through it.
The substitution is inert for what these tests pin — `repeat` windows on a
*count*, and the store is only the backing data the assertions read — and the
row callbacks are lazily evaluated outside any tracking scope, so the store's
fine-grained tracking never enters the result. Marked in the file header.

### H7. `Maybe<readonly Item[]>` collapses to `Vec<Item>`

Upstream's list accessor may return `null`/`undefined`/`false`, all of which
`this._list() || []` turns into empty (`map.ts:118`).

**Chosen:** `impl Fn() -> Vec<Item>`; a caller with nothing to show returns
`Vec::new()`. Rust has no falsy union to model.

### H8. `Item: Eq + Hash` for the identity and by-index modes

`newIndices` is a `Map` keyed by `this._key ? this._key(item) : item`
(`map.ts:251`). A JS `Map` accepts any value as a key; a Rust `HashMap` needs
`Eq + Hash`.

**Chosen:** required on `Item` for `map_array` and `map_array_by_index` (where
the key *is* the item), and on `K` only for `map_array_keyed`. A caller whose
item is not hashable — a struct containing `f64`, say — uses `map_array_keyed`
with a key they can hash. Noted because by-index mode does not *semantically*
need the bound (its matching is positional); it inherits it from sharing one
algorithm with the keyed modes, which is the faithful choice.
