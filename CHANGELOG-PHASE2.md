# Phase 2 — the async layer

Status: **foundation landed; the feature modules above it are not started.**
Read this before continuing; it is the handoff.

Upstream pin unchanged: `15975306e524f197e2231ba6bd2c259c0dc39362`.

---

## What landed

| Upstream | Here | State |
|---|---|---|
| the `Promise` / `AsyncIterable` substrate | `src/async_source.rs` | complete |
| `core/async.ts` — `handleAsync`, `asyncWrite`, `handleError`, `settlePendingSource`, `settleAutodispose`, `parkLoadingWindow` | `src/asyncrt.rs`, `src/status.rs` | complete for the promise and stream paths |
| `core/verdict.ts` — `isPending`, `latest`, `computePendingState`, companions | `src/verdict.rs` | core ladder only (see gaps) |
| `scheduler.ts` — `initTransition`, `mergeTransitionState`, `transitionComplete`, `reporterBlocksSource`, stash/restore, the incomplete-transition flush branch | `src/scheduler.rs` | complete |

Tests: **158 passing, 0 failing, 15 ignored** (was 144/0/19). Four
previously-async-blocked tests are un-ignored and green, plus eight new ones.

## The one design decision

Rust has no `Promise`, and the brief rules out an async runtime. It does not
need one: **upstream never polls a future.** It registers a continuation and
lets the platform call it. The only thing a runtime would supply is the call,
and phase 1 already ported the queue that makes calls.

So `Flight<T>` is a one-shot in-flight value settled from outside — an IO
callback, a timer, a test — and settling posts the continuation onto the
microtask queue. `Streaming<T>` is the `AsyncIterable` equivalent. The
translation table lives in `src/async_source.rs`; the important row is
`await Promise.resolve()` → `host::drive_microtasks()`.

This keeps constraint 2 exactly: no tokio, no async-std, nothing beside the
scheduler. `Async<T>` names the three shapes upstream discovers by probing the
returned object, which incidentally turns the `SYNC_NODE_RECEIVED_ASYNC`
runtime diagnostic into a type error.

## Phase 1 gaps this layer exposed (constraint 3)

Two, both real, both fixed here rather than worked around:

1. **`read()` did not add the suspension link.** Upstream's pending arms each do
   `if (!tracking && el !== c) link(el, c)` — an *untracked* read of a pending
   node still records an edge, so the source's settle sweep can reach the
   reader. Phase 1 dropped it as unreachable (nothing could be pending). It is
   load-bearing: without it `untrack(|| memo.try_get())` inside an effect
   suspends once and is never woken. Caught by
   `autodispose_contract::async_memo_read_only_via_untrack_settles_once…`,
   which is exactly the upstream test written to pin it.

2. **The transition branch of `global_flush` was a stub.** Phase 1 wrote
   `finalize_pure_queue(rt, None, false)` with a comment saying the branch was
   unreachable. It is the whole incomplete-transition protocol — stash the
   queues, detach the batch, re-stamp pending nodes, continue ambiently — and is
   now ported properly.

Everything else phase 1 kept as inert machinery (the `Transition` fields, the
queue stash shape, `Queue::notify`'s reporter registration, `REASK`,
`loading`/`loading_value`, `blocked`, `pending_sources`, the `NotReadyError`
branches in `read`) turned out to be shaped correctly and needed no rework. The
sync/async split was clean apart from the two items above — which is the
interesting finding: both gaps were places phase 1 *removed* something for being
unreachable, not places it modelled something wrongly.

## What is NOT done

The feature modules that sit on this foundation are **not started**:

| Upstream | Status |
|---|---|
| `core/lanes.ts` | not ported — `Node::transition` is honoured, but there are no optimistic lanes, no union-find merge, no parent/child companion lanes |
| `core/optimistic.ts` | not ported — no `_overrideValue`, no `createOptimistic` |
| `core/action.ts` | not ported |
| `boundaries.ts` | not ported — no `createLoadingBoundary` / `createErrorBoundary` / `createRevealOrder`. The `Queue::notify_hook` seam they install into exists and works |
| `affects.ts` | out of scope by the brief, and `verdict.rs` has no mark channel |

`verdict.rs` gaps within `isPending`/`latest`: no `markCovered` (needs
`affects`), no firewall/store-leaf deferral (needs the store port), no override
branch (needs `optimistic.ts`), no lane check in `latestRead`. The ladder's
remaining rungs — disposal guard, latest-shadow branch, held value,
`newQuestionInFlight` with `quietPending` — are ported and exercised.

Test suites named in the brief and **not** ported: `action*.test.ts`,
`createLoadingBoundary*.test.ts`, `createErrorBoundary.test.ts`,
`createOptimistic.test.ts` (5,083 lines), `createRevealOrder.test.ts`,
`createTrackedEffect.test.ts`, `affects-propagation.test.ts`,
`diagnostics.test.ts`, and the async half of `createMemo.test.ts`. The 15
remaining `#[ignore]`s name which of these blocks each one.

## Suggested order for the next pass

1. **`boundaries.ts`** — highest value per line, and the foundation is ready for
   it: `Queue::notify_hook` + `check_sources` are wired, `queue_notify` already
   forwards status up the tree, and `STATUS_PENDING` propagation works. This
   unblocks `createLoadingBoundary`, `createErrorBoundary`, and most of the
   ignored `createMemo` async tests.
2. **`lanes.ts`** — small (182 lines), self-contained, and `optimistic.ts`
   depends on it. Note the parent/child carve-out that keeps companion lanes
   from merging; it is what lets an `isPending` spinner flush before the owner's
   async settles (A14).
3. **`optimistic.ts` + `verdict.rs`'s override branch** — these move together;
   A17/A18/A24 are one story and porting the override without the verdict rules
   would give a half-model.
4. **`action.ts`** — depends on transitions (done) and lanes.
5. `affects.ts` last, and only if it comes into scope; A24's mark channel
   touches `read`, `notify_status`, `recompute` and the verdict ladder.

Read `SPEC-ASYNC-SEMANTICS.md` A17/A18/A19/A24 before step 3. The mask model
(A20/A21) is **superseded** — do not implement it; A24 is the live ruling.
