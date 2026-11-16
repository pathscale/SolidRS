# Reply to PORTING-QUESTIONS

Rulings on everything in `PORTING-QUESTIONS.md` that can be settled from this
side. Written against `872c9ad` on `port/map-array-and-repeat`
(`pathscale/SolidRS` PR #1), upstream pinned at
`15975306e524f197e2231ba6bd2c259c0dc39362`.

Four of these need work. Everything else is confirmation, and is here so the
document stops being a list of unanswered things.

**The four:** §H4 (recover the abort path), §F4 and §C1 (phase 2 has landed and
can now answer what phase 1 deferred to it), and the stale counts in §E.

---

## The general rule this port should follow

One finding in this document outranks the rest, and several open items resolve
against it once it is stated plainly.

Phase 1 was told to port machinery faithfully even where the synchronous
subset never reaches it. §G4 reports what happened: of everything phase 1 kept
as inert scaffolding, *nothing* needed rework, and the only two things phase 2
had to undo were the two places phase 1 **removed** something for being
unreachable — `read()`'s suspension link, and `global_flush`'s transition
branch stubbed with a comment calling it unreachable.

That is a clean, measured result, and it is worth more than it looks. The
instruction that mattered was not "model this carefully" but **"do not delete
anything for being unreachable."** Carrying dead machinery through a phase cost
nothing. Deleting it cost a rediscovery both times, at a point where the
original reading of the source was no longer in anyone's head.

Where an item below asks for something, it is almost always this rule applied.

---

## A. Brief vs. source discrepancies

**A1 `batch`, A2 `useContext`, A4 `tests/core/**`** — all three correct, no
change. A1's reasoning is the strongest of them: batching is structural in 2.0,
so porting a `batch` wrapper would have invented API upstream deliberately
dropped. `flush_with` is the right analogue and pinning coalescence without a
batching call is the right test.

**A3 error boundaries' sync paths.** The reading is right: the engine-side
machinery is in scope, the boundary component is not, and `notify_hook` is the
correct seam. Keep the three `#[ignore]`s.

**A5 four `async.ts` functions reachable synchronously.** Correct to port
rather than stub. A stub here would have been the §G4 mistake a third time.

---

## B. Shape changes forced by Rust

**B1 errors as values.** Right, and for the reason given: the graph must store
an error, propagate it by object identity, and re-deliver it to late readers. A
panic payload cannot carry identity without being boxed into the value the port
already uses. `Rc` + `Rc::ptr_eq` preserves the identity semantics the
propagation and silent-recovery sweeps depend on, which is the whole
requirement.

The retained panic — an error escaping every boundary, unwinding out of
`flush()` — is correct, and the `Drop` guards restoring `queue_running` and
`sync_depth` are the important half. A runtime that wedges after reporting a
halt is worse than one that does not report.

**B2 `Rc<dyn Any>`.** Both reasons hold. The second is the load-bearing one:
every path handing a value to user code must release the arena borrow first,
and a take/restore dance around each call has a hole the moment the callee
re-enters. Worth keeping the sentence about this not being "refcounting the
graph" — values are acyclic payloads, edges stay generational keys — because a
reader will otherwise assume the graph is refcounted and conclude cycles leak.

**B3 one `Node`.** Correct. Three arenas would put a tag on every edge.

**B4 the borrow rule.** The invariant is stated in the right place and the
consequence is drawn correctly: functions that can reach user code take short
borrows internally rather than receiving them. Deferring to a later safe point
would reorder cleanups, and cleanup order is observable. No change.

**B5 no arena reclamation.** Right call, and the reason is the interesting
part: `graph::unobserved` sets `DISPOSED` on a memo upstream fully expects to
resurrect, so `DISPOSED` is not proof of unreachability, and with `Copy`
handles no liveness test is sound. A wrong collector is worse than none.

Do not implement either README design speculatively. One thing to record: the
growth is proportional to nodes *ever created*, not nodes alive, so the cost
shows up in a long-lived page rather than in any test. If a host ever embeds
this behind a long-running document, that is where it will surface, and the
note should be findable from there rather than only from the README's
deviations list.

**B6 `equals` and no `Options::new()`.** Agreed, including the absence. A
constructor that silently picks a non-default comparator for a non-`PartialEq`
type is a trap that only fires in production.

**B7 setter dispatch on the node.** Correct, and it caught a real bug, which is
the argument for it.

**B8 typed cleanup returns / B9 `refresh()` as a method / B10 typed context.**
All three correct. See the note under §E about how these five tests are being
described.

---

## C. Semantics chosen where the TypeScript is ambiguous

**C1 `_error = null` vs `undefined`.** ⚠️ **This one has a live follow-up.**

Collapsing both to `Option::None` was right for phase 1: `outgoingError` is
only read under `STATUS_ERROR`, where `_error` is a real error object. But the
entry itself says the async port should re-check when `parkLoadingWindow`
lands, "which does set `null` under `STATUS_ERROR`-adjacent states."

**Phase 2 has landed.** Either that re-check happened, in which case say so and
close the item, or it did not, in which case do it now. This is exactly the
shape of thing that goes stale silently: a correctly-flagged deferral whose
trigger fired without anyone noticing.

**C2 previous value cloned across the computation.** Correct, and the note that
the earlier take-and-restore approach *did* have a behavioural difference is
worth keeping. It is the evidence for the current design.

**C3 `prev_value` rolls forward on both arms.** Matches upstream's `finally`.
Correct.

**C4 `staleValues` for render effects.** Ported as written rather than dropped.
Correct, and per the general rule above, this is the right instinct — `read()`
branches on it, so dropping it would have been a rediscovery.

**C5 `queue_notify` returns `false` at the root.** Ported exactly. Correct,
including the consequence that a render-effect compute error with no boundary
panics.

**C6 `is_empty_batch` omits `transient_store_nodes`.** Omitting rather than
stubbing with a permanently-empty set is right: an always-empty set reads as a
live check that passes, which is harder to find later than an absent one.
Recorded well.

---

## D. Machinery ported but inert

No change, and this section should not shrink. The table of ported-with-no-
producer machinery plus the `finalize_pure_queue` comment block listing the
five empty gates *in call order* is what makes the later ports cheap. §G4 is
the receipt.

The invariants split is right too. INV-2/3/4/5/6/9/10 being documented with the
port that re-arms each — rather than deleted for being inert — means the same
thing.

---

## E. Test-porting decisions

**E1–E5, E7.** All correct. E1 is the one worth keeping visible: several
upstream `graph.test.ts` tests rely on eager memo creation, and adding a read
to "improve" the port trips the ownerless auto-dispose tail. `let _ = memo;`
not `memo.get();` is the kind of thing that gets helpfully broken by a future
contributor.

E2's forward slot is right — creation order is what the test measures, so
reordering the declarations would change the expected string and quietly
weaken the test into one that passes for the wrong reason.

**E6 ignore granularity.** The choice is right: two describe-level markers
beat forty near-identical stubs. But it has a reporting consequence that needs
stating in the document, because the number is being quoted.

`cargo test` reports **15 ignored**. Two of those fifteen —
`describe_async_compute` and `describe_is_pending_and_latest`, both in
`tests/create_memo.rs` — each stand for an entire upstream `describe` block.
So fifteen ignored markers do not mean fifteen unported upstream tests; the
true number of unported upstream cases is larger and is not recoverable from
the count. Say so in §E6 explicitly. Anyone quoting "15 ignored" without that
sentence is overstating coverage, and the first person to do it was me.

**Stale counts.** §E opens with "132 pass, 19 are `#[ignore]`d". Current
reality is **183 passing, 0 failing, 15 ignored**. Fix the line, and prefer
phrasing that will not rot — or state the commit the count was taken at.

**The taxonomy, confirmed.** I grepped the fifteen and they fall out as:

| category | count | where |
|---|---|---|
| blocked on `boundaries.ts` | 5 | `create_effect` 1, `create_memo` 3, `on_cleanup` 1 |
| blocked on `lanes.ts` | 2 | `create_signal` 2 |
| staged for phase 2 | 3 | `create_effect` 1, `create_memo` 2 (the describe markers) |
| **not representable** | **5** | `create_effect` 1, `create_memo` 3, `create_reaction` 1 |

Confirm or correct this split. The last row is the one being reported as a
result: five upstream tests that **cannot be written** here, because each
constructs a value Rust forbids — a cleanup that is not callable (B8), a handle
the constructor never branded (B9). Upstream must check those at run time. Here
the check is unrepresentable, which is the strongest form of passing, and the
only one that leaves nothing to execute.

That claim rests on my reading of your `#[ignore]` reasons. If any of those
five is really "hard to express" rather than "cannot be expressed", it does not
belong in that row and I need to know.

---

## F. Open questions for upstream

These are genuinely upstream's to answer, and all four are correctly recorded
as *ported verbatim pending an answer*, which is the right default. Nothing to
change in three of them.

**F1 `heap.ts:71` insertion early-out.** Follow the source. The `#2692` tests
pass either way, so there is nothing to decide locally.

**F2 `core.ts:340` redundant `!isEffect` guard.** Ported verbatim in case an
effect can acquire a comparator through an uncovered path. Correct — a guard
removed for looking redundant is the §G4 mistake in miniature.

**F3 `graph.ts:109` `_pendingObserver` labelling.** Inert here. Follow the
source.

**F4 `scheduler.ts:492` assignment order.** ⚠️ **Answerable now.** The entry
says "unreachable synchronously; the async port should confirm which order is
intended." The async port has landed. Either it confirmed and the entry should
record the answer, or the incomplete-transition path still does not exercise
it, and the entry should say *that* instead — those are different states and
right now the document cannot distinguish them.

This one matters more than the other three: it is a real ordering bug if the
batch was replaced and the fresh one is empty, and it is the only F-item where
a wrong answer produces wrong behaviour rather than dead code.

---

## G. Phase 2 judgement calls

**G1 no async runtime.** Correct, and the analysis is the strongest single
observation in this document: upstream never *polls* a future, it registers a
continuation and the platform calls it, so a runtime would contribute only the
call — and phase 1 already ported the queue that makes calls. `Flight` and
`Streaming` settled from outside is the right shape. Keep the note that
`SYNC_NODE_RECEIVED_ASYNC` becomes unrepresentable; it belongs with the other
four in §E's "not representable" row conceptually, even though it is a
diagnostic rather than a test.

**G2 spec vs implementation.** Implementation wins, per constraint 4. Correct.
Worth flagging to upstream separately: `SPEC-ASYNC-SEMANTICS.md` A19 says an
uninitialized source's `NotReadyError` must propagate, but `read()` raises only
with a reading context *or* `STATUS_UNINITIALIZED`, so an initialized pending
node with no observer returns stale. One of the two documents is wrong and it
is worth telling them which you followed.

**G3 `_inFlight` cleared only by `recompute`.** Correct, and "found the hard
way" is why the comment in `asyncrt.rs` should never be deleted: clearing on
first landing closes every stream after one yield, and nothing about the code
says so.

**G4 the two undone simplifications.** No change. See the general rule at the
top — this entry is its evidence, and it should be cited from anywhere someone
argues for dropping inert machinery.

---

## H. `map.ts`

### H1 four overloads to three named functions — **accepted**

`map.length > 1` is not introspectable and there is no way to recover it, so
the API has to name what JS infers.

One correction to the framing. The consequence is described as "index-taking
forms always allocate an index signal", presented as a cost. It mostly is not:
a caller who reached for an index-taking function asked for the index. The gap
only opens for a caller who takes the index and ignores it, which is visible at
their call site and is their choice. Record it as a shape difference. The
future `map_array_values` is the right eventual answer and is not urgent.

### H2 `Rc<Vec<M>>` compared by `Rc::ptr_eq` — **accepted**

Right, and right for the right reason. Upstream's array identity is not an
optimisation, it is the mechanism that stops downstream memos re-running, so a
port allocating a fresh array every pass would be semantically wrong in a way
that tests on values alone would never catch. `Rc::ptr_eq` against upstream's
default `===` is an exact match, and
`an_unchanged_pass_keeps_the_mapped_array_identity` is the right pin.

`M: Clone` is the correct price, and the escape hatch — map to `Rc<T>` and the
clone is a refcount bump — is the right advice. Rejecting `Vec<Rc<M>>` because
it leaks `Rc` into every callback signature is also right.

**One thing to confirm:** a pass with no structural change must clone
*nothing*. The entire point is that it returns the same `Rc`, so retained rows
are never moved out and never copied. If the implementation clones retained
rows on the unchanged path, the bound is buying nothing and the cost becomes
per row per pass rather than per row per structural change. The identity test
proves the `Rc` is reused; it does not prove no rows were cloned on the way to
deciding that. Add the assertion, or say why it is already impossible.

### H3 fusing `a === b || compare(key, a, b)` — **accepted**

The `PartialEq` argument is the strong half: demanding a bound upstream's API
does not need would be a port that changed its own interface, which is worse
than a redundant comparison.

**Write down the assumption.** The fusion is sound because the key function is
pure: if `key` is pure then `a == b` implies `key(a) == key(b)`, so the
disjunct is unreachable and dropping it changes nothing. Hand it an impure key
and upstream's reference short-circuit still matches the row while the fused
predicate does not. Upstream does not document key purity as a requirement, so
this port is relying on something unstated. Put it in H3 as an explicit
assumption rather than leaving it implicit in the derivation.

The row-signal equality note (structural, not referential) is a genuinely
different reading and is handled correctly: the divergence is observable only
through reference identity, which Rust values do not have. Good that it is
stated rather than buried.

### H4 the unreachable abort path — **change this one**

The reasoning is sound and the conclusion is half right. Staging is ported,
the observable ordering consequence is ported and pinned, and the abort arm is
unreachable because public mappers are infallible. All true.

But compare it against §G4, in this same document. Phase 1 deleted two things
for being unreachable and phase 2 had to put both back, each time re-reading
upstream to recover a reading someone had already done. `try_map_array` plus
loading boundaries makes this arm reachable. The same rediscovery is already
scheduled.

I am not asking for speculative untested code — "an untested abort path is
worse than a documented absent one" is correct as stated. I am asking that the
absence be *recoverable*. Either:

- port the arm with its failure branch present but unconstructible, so the
  structure survives even though no test can enter it; or
- keep the comment, but make it name the upstream file and line the arm comes
  from, the exact condition that makes it reachable (`try_map_array` plus a
  loading boundary), and what would be observably wrong if the arm were absent
  when it became reachable.

Whichever is cheaper. "Unreachable, see #2903" cannot be rebuilt from; a
comment naming the source location and the reachability condition can. Note
that §D already does this correctly for eleven other pieces of machinery —
`finalize_pure_queue`'s comment block lists five empty gates in call order.
H4 should meet the standard the rest of the port already sets.

### H5 `$TRACK` omitted — **accepted**

`list` is a plain accessor here and reading it tracks normally. The store port
reintroduces it. No change.

### H6 three `repeat` tests sourcing from `createStore` — **accepted**

The substitution is inert for what those tests pin: `repeat` windows on a
count, the store is only backing data the assertions read, and the row
callbacks are lazily evaluated outside any tracking scope so the store's
fine-grained tracking never enters the result. Marking it in the file header
is the right disclosure.

### H7 `Maybe<readonly Item[]>` to `Vec<Item>` — **accepted**

Rust has no falsy union. `Vec::new()` for "nothing to show" is the only
sensible collapse.

### H8 `Item: Eq + Hash` — **accepted**

Required where the key *is* the item, on `K` alone for `map_array_keyed`, with
the documented escape for an unhashable item. The note that by-index mode does
not semantically need the bound and inherits it from sharing one algorithm is
exactly the sort of thing that should be recorded — it tells a future reader
the bound is incidental there, so removing it later is a refactor rather than a
semantic change.

---

## Summary of what to do

1. **H4** — make the abort path's absence recoverable, by either of the two
   routes above.
2. **C1** — phase 2 has landed; re-check the `null` vs `undefined` collapse
   against `parkLoadingWindow`, or record that it was checked.
3. **F4** — phase 2 has landed; record what it found about `scheduler.ts:492`,
   or record that the path is still not exercised. They are different answers.
4. **§E** — fix "132 pass, 19 ignored" to the current 183/0/15, state in E6
   that two of the fifteen are describe-level markers, and confirm or correct
   the four-way taxonomy above.
5. **H3** — record key purity as an explicit assumption.
6. **H2** — confirm the unchanged path clones nothing.

Nothing else needs changing. The rest of this document is a good record and
should be kept as it is.
