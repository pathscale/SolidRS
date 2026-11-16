# solid_rs

A faithful Rust port of the **synchronous core plus the async foundation** of
[`@solidjs/signals`](https://github.com/solidjs/solid/tree/next/packages/solid-signals).

**Pinned upstream commit:** `15975306e524f197e2231ba6bd2c259c0dc39362`
(`@solidjs/signals@2.0.0-rc.0`, branch `next`, directory
`packages/solid-signals`).

Semantics come from the TypeScript, not from other Rust reactive libraries.
Where the TypeScript shape did not survive translation, the *shape* changed and
the semantics did not; every such decision is in
[PORTING-QUESTIONS.md](PORTING-QUESTIONS.md).

```rust
use solid_rs::{create_signal, create_memo, create_effect, create_root, flush};

let (count, set_count) = create_signal(0);
let doubled = create_memo(move |_| count.get() * 2);

create_root(move |_dispose| {
    create_effect(
        move |_| doubled.get(),                  // compute: tracked
        move |v: &i32, _| println!("{v}"),       // apply: untracked, after the flush
    );
});
flush();

// A write stages; the tick is the unit of truth for values.
set_count.set(5);
assert_eq!(count.get(), 0);
flush();
assert_eq!(count.get(), 5);
```

---

## Phase status

Phase 1 (synchronous core) is complete. Phase 2 (the async layer) has its
**foundation** landed — async computeds suspend and settle, transitions open and
complete, `is_pending`/`latest` work — but the feature modules above it
(boundaries, optimistic, lanes, actions) are **not started**. See
[CHANGELOG-PHASE2.md](CHANGELOG-PHASE2.md) for exactly what is and is not there,
and the recommended order for the next pass.

## What is ported

The whole synchronous reactive engine, not a reimplementation of its surface.

| Upstream | Here |
|---|---|
| `core/core.ts` | `src/core.rs` |
| `core/graph.ts` | `src/graph.rs` |
| `core/owner.ts` | `src/owner.rs` |
| `core/effect.ts` | `src/effect.rs` |
| `core/scheduler.ts` | `src/scheduler.rs` |
| `core/heap.ts` | `src/heap.rs` |
| `core/context.ts` | `src/context.rs` |
| `core/error.ts` | `src/error.rs` |
| `core/constants.ts` | `src/constants.rs` |
| `core/types.ts` (+ `Link`) | `src/node.rs` |
| `core/invariants.ts` | `src/invariants.rs` (feature `test_invariants`) |
| the synchronous half of `core/async.ts` | `src/status.rs` |
| the synchronous surface of `signals.ts` | `src/signals.rs` |
| the `Promise` / `AsyncIterable` substrate | `src/async_source.rs` |
| `core/async.ts` suspension machinery | `src/asyncrt.rs` |
| `core/verdict.ts` (core ladder) | `src/verdict.rs` |
| `map.ts` (`mapArray`, `repeat`) | `src/map.rs` |

Public API: `create_signal`, `create_signal_with`, `create_memo`,
`create_memo_with`, `try_create_memo_with`, `create_computed_signal`
(upstream's `createSignal(fn)`), `create_effect`, `create_effect_with`,
`try_create_effect`, `create_render_effect`, `create_render_effect_with`,
`create_reaction`, `create_root`, `create_root_with`, `untrack`, `flush`,
`flush_with`, `on_cleanup`, `on_dispose`, `get_owner`, `get_observer`,
`run_with_owner`, `create_context` / `get_context` / `set_context`,
`map_array` / `map_array_by_index` / `map_array_keyed`, `repeat`,
`Memo::refresh`, `reset_error_halt`, `get_next_child_id`, `peek_next_child_id`,
and the `host` and `debug` modules.

The engine keeps the parts that only pay off later, because removing them would
mean re-deriving where each branch belonged: the height-ordered dirty and zombie
heaps, the zombie/pending-commit disposal protocol, the transaction-shaped
batch, the queue tree with its notification chain and stash/restore shape, the
`REASK` classification, and the full `CONFIG_*` / `REACTIVE_*` / `STATUS_*` bit
layout at its original bit positions.

## What is **not** ported

Out of scope by the brief; each is a separate port:

- `core/lanes.ts`, `core/optimistic.ts`, `core/action.ts` — optimistic lanes,
  `createOptimistic`, actions. Transitions themselves **are** ported.
- `boundaries.ts` — `createLoadingBoundary`, `createErrorBoundary`,
  `createRevealOrder`. The `Queue::notify_hook` slot is the seam these install
  into; the engine-side error and pending machinery they sit on top of **is**
  ported.
- The parts of `core/verdict.ts` that need the above: mark coverage, the
  firewall deferral, the override branch, the lane check in `latest`.
- `affects.ts`, `store/**` (the proxy store).
- `core/dev.ts` diagnostics beyond messages, `core/external.ts`, snapshot
  capture, and `createTrackedEffect` / `onSettled` (leaf-owner primitives whose
  contract is defined against the async settle).

Two names in the brief do not exist upstream at this commit: `batch` (batching
is structural in 2.0 — writes stage and commit at flush) and `useContext` (the
package exports `getContext`/`setContext`; `useContext` lives in `solid-js`).
Details in `porting-reply.md` §A (open items only are in PORTING-QUESTIONS.md;
the full original record is `git show d6798ed:PORTING-QUESTIONS.md`).

## Design

**Arena, not `Rc<RefCell<Node>>`.** A reactive graph is cyclic by construction —
a dependency points at its subscribers and every subscriber points back.
Refcounting that never reaches zero. Nodes and links live in `slotmap` arenas
addressed by generational keys, so cycles are harmless (a stale key is a miss,
not a dangling read) and disposal is an explicit, auditable step rather than an
emergent property of drop order.

**Two arenas, one borrow rule.** Nodes and links are separate `RefCell`s
because `link()` allocates an edge while node fields are being read, and the
dispose/trim walks free edges while iterating them. Every mutable field of a
node carries its own cell, so a *shared* borrow covers even the recursive walks
that mutate one node while iterating another's list. The rule, stated once in
`runtime.rs`: a borrow may span engine-internal work, but must be dropped before
user code runs or a node is allocated. Every function that can reach user code
is written as *borrowed prologue → drop → user call → borrowed epilogue*, which
is the shape the TypeScript already has.

**Errors are values.** A computation returns `Result<T, ReactiveError>` instead
of throwing. `ReactiveError` keeps its payload behind an `Rc` and compares with
`Rc::ptr_eq`, because the propagation and silent-recovery sweeps are correct
only under *object identity*, never structural equality. The one place a panic
survives is an error that escapes every boundary — upstream rethrows there too.

**Single-threaded.** Nothing is `Send` or `Sync`, nothing locks, there is no
async runtime. The runtime is thread-local; a graph belongs to the thread that
created it.

## Async

A computation returns [`Async<T>`]: `Ready` (already available), `Flight`
(in flight), or `Stream` (a sequence). Rust has no `Promise`, and no runtime is
needed — upstream never polls a future, it registers a continuation and lets the
platform call it. `Flight` is settled from outside and posts its continuation
onto the microtask queue, so `await Promise.resolve()` in an upstream test maps
to `host::drive_microtasks()`.

```rust
use solid_rs::*;
let (flight, resolver) = Flight::<i32>::new();
let data = create_root(move |_| {
    let f = flight.clone();
    create_async_memo(move |_| Async::Flight(f.clone()))
});

assert!(data.try_get().is_err());   // suspended, not guessing
resolver.resolve(42);
host::drive_microtasks();
assert_eq!(data.get(), 42);
```

An uninitialized source is *loading*, not pending: `is_pending` reads `false`
because there is no observable value yet to be non-final. A refetch — a new
question with a stale value still showing — is pending.

## Platform support

Verified on `aarch64-apple-darwin` and **`wasm32-unknown-unknown`**:

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown
```

Both targets build warning-free in every feature configuration (default,
`--no-default-features`, `--features test_invariants`). Nothing about the port
is host-specific: the dependency surface is one crate (`slotmap`, pure Rust),
and there is no use of `std::time`, `std::thread`, `std::net`, `std::fs`,
`std::process`, `std::env`, `std::io` or `std::sync` anywhere in `src/`. The
design already assumed a single-threaded, host-driven world — no `Send`/`Sync`,
no locks, no async runtime, and a microtask queue the host pumps — which is the
shape a wasm embedding wants; `host::drive_microtasks()` maps directly onto
`queueMicrotask`.

One behavioural caveat rather than a build one: `wasm32-unknown-unknown` has no
stderr, so the six `eprintln!` diagnostics (`REACTIVITY_HALTED`, the unhandled
effect-error log, the dev `NO_OWNER_*` warnings) compile but go nowhere. An
embedding that wants to see them needs a log shim; nothing else changes.

### It runs, not just compiles

Building an rlib never links, so "compiles for wasm32" is a weaker claim than
it sounds: the failures that actually bite on this target — unsupported `std`
surface, panic handling, a stray syscall import — only surface at *link and
instantiate* time. `crates/solidrs-wasm-smoke/` closes that gap. A `cdylib`
guest is built to a real `.wasm` and instantiated under
[wasmi](https://github.com/wasmi-labs/wasmi):

```bash
crates/solidrs-wasm-smoke/run.sh
```

| | shipping (`--no-default-features`) | with `dev` |
|---|---|---|
| module size | 116,260 B (113.5 KiB) | 119,302 B (116.5 KiB) |
| host imports | **none** | **none** |
| `run` (signal → memo → effect) | 40 ✓ | 40 ✓ |
| `run_async` (flight suspends, settles via microtask) | 7 ✓ | 7 ✓ |
| `run_dispose` (owner teardown runs cleanups) | 2 ✓ | 2 ✓ |

"Host imports: none" is the load-bearing line — the reactive core needs no
WASI, no JS glue, no host functions of any kind. It is a closed module.

> **Rule: `solid_rs` must never depend on `wasmi` — not as a dependency, not as
> a dev-dependency, not transitively.** The interpreter belongs to the *host*
> that instantiates the module, never to the library under test. This is
> enforced structurally, not by convention: `crates/solidrs-wasm-smoke` is a
> separate workspace (`exclude`d in `Cargo.toml`), so nothing in
> `cargo tree -p solid_rs` can reach it, and `run.sh` re-asserts it on every
> run with both default and `--all-features`.

## Driving updates

There is no JS event loop, so the microtask queue is ours to provide.

- `flush()` is the explicit synchronous drain — what tests and any caller that
  needs to *observe* a write use.
- `host::drive_microtasks()` is **the one place a host drives it**: call it at
  the end of a host task (after an event handler returns, at a frame boundary)
  and you get upstream's batching — several writes in a turn collapse into one
  update pass. A host that never calls it just has to call `flush()` itself.

## Deviations from upstream

One, and it is deliberate: **arena slots are never reclaimed.**

Upstream leaves unreachable nodes to the JS garbage collector. An arena has
none, and two facts make an automatic collector unsound here:

1. `DISPOSED` is not proof of unreachability. `graph::unobserved` sets it on an
   auto-disposing memo that upstream fully expects to resurrect on the next read
   (`prepare_computed` recomputes a disposed node). Freeing on that flag turns a
   legal read into a miss.
2. Public handles are `Copy`, so the runtime cannot know whether user code can
   still name a node.

A reclamation pass was written and then removed rather than shipped subtly
wrong.

**What this costs an embedder.** The arena grows with the number of nodes
**ever created**, not the number alive. Disposing a subtree does not give its
slots back, so a document that mounts and unmounts — a list that refilters, a
route that remounts, anything driven by a stream of updates — grows without
bound for as long as it is open. Memory tracks the total churn, not the size of
what is on screen.

The case that finds this is a single document held open for days, which is
exactly the case a test suite does not have: every test builds a graph, asserts,
and exits. No amount of green tests says anything about it. If you are
embedding, this is the number to watch, and `debug::node_count()` is how to
watch it.

Either of these would fix it, and both are follow-on work rather than porting
work:

- make handles `Clone`-with-`Drop` (refcount the *handle*, not the graph — edges
  stay generational keys, so cycles remain collectable), or
- scope the arena, so a `create_root` subtree's slots are released with the root.

`debug::node_count()` exposes cumulative node-slot occupancy; dependency links
are reclaimed during disposal, and `debug::link_count()` reports their current
occupancy. The `arena_churn` integration test pins the distinction: 1,000
create/flush/dispose cycles retain 4,000 node slots and zero dependency links.

Two smaller, fully-documented shape changes: cleanup return values and
`refresh()` targets are statically typed, so three upstream *runtime* errors are
compile errors here (`porting-reply.md` §B8, §B9).

## Verification

```bash
cargo test                              # 172 executable + 16 doc tests pass; 15 ignored
cargo test --features test_invariants   # + the INV-* self-consistency checks
cargo clippy --all-targets              # clean
```

Ported suites, each keeping its upstream test names (in a doc comment above
every test, so a Rust failure greps straight back to the TypeScript):

| Upstream file | Here | Passing | Ignored |
|---|---|---:|---:|
| `graph.test.ts` | `tests/graph.rs` | 21 | 0 |
| `createRoot.test.ts` | `tests/create_root.rs` | 22 | 0 |
| `createMemo.test.ts` (sync subset) | `tests/create_memo.rs` | 25 | 8 |
| `createEffect.test.ts` | `tests/create_effect.rs` | 21 | 3 |
| `createSignal.test.ts` | `tests/create_signal.rs` | 8 | 2 |
| `onCleanup` + `getOwner` + `runWithOwner` | `tests/on_cleanup.rs` | 8 | 1 |
| `flush.test.ts` | `tests/flush.rs` | 7 | 0 |
| `context.test.ts` | `tests/context.rs` | 7 | 0 |
| `createReaction.test.ts` | `tests/create_reaction.rs` | 4 | 1 |
| `untrack.test.ts` | `tests/untrack.rs` | 3 | 0 |
| `autodispose-contract.test.ts` | `tests/autodispose_contract.rs` | 6 | 0 |
| `mapArray.test.ts` | `tests/map_array.rs` | 12 | 0 |
| `repeat.test.ts` | `tests/repeat.rs` | 11 | 0 |
| — (smoke) | `tests/smoke.rs` | 4 | 0 |

A test that cannot be ported is `#[ignore]`d with a one-line reason, never
deleted. All 19 fall into three buckets: needs an async source, needs the
verdict layer (`isPending`/`latest`), or is not representable because Rust's
types make the upstream failure mode a compile error.

`tests/core/**` contains no tests — only `reactivity.bench.ts`, a Vitest
benchmark whose shapes `tests/graph.rs` already covers.

## Features

- `dev` *(default)* — upstream's `__DEV__`: the
  `PRIMITIVE_IN_FORBIDDEN_SCOPE` / `REACTIVE_WRITE_IN_OWNED_SCOPE` guards,
  owner names, and diagnostic messages.
- `test_invariants` — upstream's `__TEST__`: the INV-* self-consistency checks.
  Gated separately from `dev` because upstream measured them at 5-21% on their
  benchmark suite.

## Licence

MIT, matching upstream.
