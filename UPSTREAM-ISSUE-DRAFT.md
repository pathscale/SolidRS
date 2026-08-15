# DRAFT — NOT FILED, AND NOT TO BE FILED WITHOUT THE OWNER'S GO-AHEAD

This is issue text for `solidjs/solid`, written while the reading was fresh and
held here deliberately. **Nothing has been filed.** Do not open it, do not paste
it into an existing thread, and do not link it from anywhere public until the
repository owner says so.

**Why it is held.** The findings below are described in a paper submitted to a
double-blind venue. A public issue under an identifiable account, containing the
same findings in the same words, is a searchable link between the anonymous
submission and the filer. That risk is the owner's to weigh, not the drafter's,
and it expires when the review period closes.

**One issue, not four.** These came from one pass over one file set at one
commit, and a maintainer can answer them together far more cheaply than
separately.

**A note on F4 below.** The instruction that produced this draft named F1, F2,
F3 and the A19 divergence. F4 is included anyway, flagged, because it is the
only one of the five where a wrong answer produces wrong behaviour rather than
dead code — so its omission looked more like a slip than a decision. Deleting a
section before filing is cheap; noticing a missing one is not. Drop §4 if it was
deliberate.

---

## Title

Four points in the `solid-signals` scheduler and graph where the source and its
stated intent disagree

## Body

While porting `packages/solid-signals` to Rust I read the synchronous core and
the async layer closely against commit
`15975306e524f197e2231ba6bd2c259c0dc39362` (branch `next`). Four places came up
where I could not tell intent from the source, and in each case I followed the
source exactly rather than guessing. They are ordered by how much a wrong answer
costs, not by file.

None of these is a bug report. Three are questions, and if the answer is "that
is intended" then the useful outcome is a comment in the source saying so, since
I am unlikely to be the last person to stop on them.

---

### 1. `heap.ts:71` — a manual write can strand a stale height

`insertIntoHeap` returns early on `REACTIVE_MANUAL_WRITE`, and
`insertIntoHeapHeight` early-outs on the same flag. So a node that is manually
written *and* whose height changed never has the height adjustment applied until
the flag clears at commit.

I could not construct a case that discriminates the two orderings, because doing
so requires knowing whether "manually written, height changed, observed before
commit" is a combination that is meant to be reachable at all. If it is not,
this is fine and the second early-out is load-bearing. If it is, this is a
missed reorder that will surface as a node recomputing at the wrong height under
a specific write pattern, which is the kind of thing that shows up once in
production and never in a test.

**Question:** is the height adjustment deliberately deferred to commit, or
should the second early-out be conditional?

This is the one I would most like an answer to.

---

### 2. `scheduler.ts:492` — `scheduled` is read after the batch may have been replaced

```js
// :479 — the batch may be replaced here
// :492
scheduled = dirtyQueue._max >= dirtyQueue._min || this._batch._pendingNodes.length > 0;
```

Line 492 reads `this._batch` after line 479 may have swapped in a fresh
`Transition`. A fresh batch has an empty `_pendingNodes`, so the second disjunct
is trivially false whenever the replacement happened — and `scheduled` is what
decides whether another flush gets queued.

Two readings, and the source does not distinguish them:

* **intended** — the replacement means the old batch's pending nodes have been
  stashed and are no longer this flush's business, so reading the fresh empty
  batch is correct; or
* **an ordering bug** — the read was meant to observe the batch as it stood at
  :479, and replacing first drops a wakeup, leaving work stranded until an
  unrelated write happens to reschedule.

In the port I kept your order. I also found that inverting it leaves the entire
rest of my test suite green, which is why I am asking rather than assuming: the
branch is executed by tests, but nothing in either codebase appears to assert
the behaviour that distinguishes the two readings.

**Question:** is reading the post-replacement batch intended?

---

### 3. `graph.ts:109` — `_pendingObserver` is labelled by the latest read

The comment explains that outside a recompute there is no pass boundary, so the
latest read labels the field. That makes the label order-dependent for untracked
probe reads: two probes issued in different orders leave different observers in
the field.

Nothing I ported reads the field in a way that depends on which of the two it
holds, so this is inert for me. I am flagging it because it stops being inert
the moment a reader is added that cares — diagnostics, or the ownership-tracking
half of `boundaries.ts`, both look like candidates.

**Question:** is the order-dependence intended, or an artefact of there being no
generation counter to compare against?

---

### 4. `SPEC-ASYNC-SEMANTICS.md` A19 disagrees with `read()`

A19 says an uninitialized source's `NotReadyError` must propagate. `read()`
raises only when there is a reading context *or* `STATUS_UNINITIALIZED`, so an
initialized pending node with no observer returns a stale value instead of
propagating.

One of the two documents is wrong. I followed the implementation, on the
principle that shipped behaviour beats prose, but which one you intend changes
what a correct port of the async layer looks like.

**Question:** should A19 be narrowed, or should `read()` propagate in that case?

---

### 5. `core.ts:340` — a redundant guard (footnote, no action needed)

`(!isEffect && wasUninitialized) || !el._equals || …` gates the uninitialized
short-circuit on `!isEffect`, but effects always carry `_equals: false`, so the
second clause already forces `true` for them.

I kept it, on the grounds that "every effect has `_equals: false`" is an
invariant nothing enforces, so removing the guard is safe today and silently
wrong the first time an effect acquires a comparator. Mentioned only so nobody
re-derives it; no answer needed unless you want it gone.

---

## What I am not asking for

I am not asking for these to be fixed, and I am not reporting a user-visible
defect — everything above is either inert or indistinguishable in behaviour from
its alternative under the tests that exist. What is useful to me is knowing
which readings are intended, so a port does not encode a guess as a fact.
