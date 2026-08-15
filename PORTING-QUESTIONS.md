# Porting questions — open items

Upstream pin: `15975306e524f197e2231ba6bd2c259c0dc39362`
(github.com/solidjs/solid, branch `next`, `packages/solid-signals`).

This file used to hold every judgement call made during the port, resolved and
unresolved together (§A–§H, 678 lines). It has been cut down to **only what is
still open**.

**Where the resolved record went.** Nothing was destroyed:

* the *rulings* are in [`porting-reply.md`](porting-reply.md), section by section;
* the *rationale* is in the code at the site it applies to — `src/map.rs`'s
  `#2903` contract block and its three `ABORT SITE` markers, `src/asyncrt.rs`'s
  note on why `in_flight` is not cleared in `async_write`, `src/verdict.rs`'s
  pending ladder, `src/graph.rs`'s borrow rule;
* the *full prior text* is `git show d6798ed:PORTING-QUESTIONS.md`.

Four items are still open. Three are questions about **upstream's intent** that
no amount of local work can settle; one needs a decision from this project's
owner. Two items previously listed as open are now closed, and are recorded as
such below so their absence is not read as an oversight.

---

## Closed since the last revision

### C1 — `_error = null` vs `_error = undefined`

**Closed: the collapse is safe.** Phase 1 mapped both onto a single
`Option::None` and deferred the re-check until `parkLoadingWindow` existed. It
now does, at [`src/asyncrt.rs:307`](src/asyncrt.rs#L307).

Upstream only ever distinguishes the two by reading the field back and comparing
against `undefined`. This port has exactly one read that could care —
[`src/core.rs:267`](src/core.rs#L267) — and it is gated on `STATUS_ERROR`. So
the collapse is unsafe if and only if `error` can be `None` while `STATUS_ERROR`
is set. It cannot:

| write of `None` | why it cannot coincide with `STATUS_ERROR` |
| --- | --- |
| `src/status.rs:63` (`set_pending_error` with `source = None`) | its only reaching call site, `status.rs:447`, is guarded by `if !errored`, where `errored = has_status(ERROR)` |
| `src/status.rs:133` (`clear_status`) | the same borrow clears `status_flags` to `0`/`UNINITIALIZED` *before* nulling the field, so the pair is atomic |

Every other write — `effect.rs:283`, `status.rs:68`, `:77`, `:198`, and
`asyncrt.rs:307` itself — stores `Some`. `parkLoadingWindow` in particular
stores `Some` *and* is guarded by `if !errored`, so the path this re-check was
deferred for does not introduce the case.

### F4's *measurement* — is `scheduler.ts:492`'s branch exercised? **Yes, and it was unpinned.**

The question itself is still open (below); what is closed is the empirical part.

**A previous revision of this entry reported zero probe hits and concluded the
branch was dead. That was a measurement error**, recorded here because the
mistake is easy to repeat: `cargo test` captures stderr for *passing* tests, so
an `eprintln!` probe inside code that only runs in passing tests prints nothing
to the captured output. The probe was fine; reading its result without
`--nocapture` was not.

Re-measured, with an `eprintln!` probe on the incomplete-transition branch at
[`src/scheduler.rs:840`](src/scheduler.rs#L840):

```
cargo test                        -> 0   (wrong: output captured)
cargo test -- --nocapture         -> 4
```

Three pre-existing async tests reach it — `a_superseded_flight_does_not_clobber_a_newer_one`,
`is_pending_is_false_during_the_initial_load`, and
`should_return_stale_value_when_reading_re_pending_async_computed` — plus the
characterisation test added for it.

**Executed is not the same as pinned, and that distinction is the finding.**
Inverting the order — computing `scheduled` *before* the batch is replaced,
which is the other reading of `scheduler.ts:492` — leaves all 186 pre-existing
tests passing. Only the new test fails. So the three tests above walk through
the branch without asserting anything about the behaviour in question, and until
that test existed an unrelated tidy-up of `global_flush_body` could have
silently answered an open question.

---

## Still open

### F1 — `heap.ts:71`: `insertIntoHeap`'s `REACTIVE_MANUAL_WRITE` early-out

`_insert` returns early on `REACTIVE_MANUAL_WRITE`, but `insertIntoHeapHeight`
early-outs on the same flag. A manually-written node whose *height* changed
therefore never gets its height adjusted until the flag clears at commit.

* **Port's behaviour:** verbatim, both early-outs preserved. See `src/heap.rs`.
* **Why it is undecidable locally:** the `#2692` tests pass under either
  ordering, so the suite cannot discriminate. Constructing a case that does
  means producing a manual write that *also* changes height and is observed
  before commit — which requires knowing whether that combination is meant to be
  possible at all.
* **What would settle it:** upstream saying whether the height adjustment is
  meant to be deferred to commit (intended) or whether the second early-out
  should be conditional (a latent missed reorder).

### F2 — `core.ts:340`: the redundant `!isEffect` guard

```ts
(!isEffect && wasUninitialized) || !el._equals || …
```

The uninitialized short-circuit is gated on `!isEffect`, but effects always
carry `_equals: false`, so the second clause already forces `true` for them. The
guard looks vestigial.

* **Port's behaviour:** verbatim, guard retained.
* **Why it is retained:** the redundancy is a property of *today's* call graph —
  "every effect has `_equals: false`" is an invariant nothing enforces. Deleting
  the guard would be safe now and silently wrong the first time an effect
  acquires a comparator.
* **Cost of a wrong answer:** zero either way, today. Lowest-stakes item here;
  listed only so the reasoning is not re-derived.
* **What would settle it:** upstream saying whether the guard is defensive or
  dead.

### F3 — `graph.ts:109`: `_pendingObserver` labelling outside a recompute

The comment reads: outside a recompute there is no pass boundary, so the latest
read labels it. That makes the label **order-dependent for untracked probe
reads** — two probes in different orders leave different observers in the field.

* **Port's behaviour:** verbatim, in `src/graph.rs`.
* **Status here:** inert. The port has no probe reads, so nothing observes which
  of the two candidate observers the field holds.
* **Why it stays on the list rather than being dropped:** it goes live the
  moment a reader is added that cares. Diagnostics and the ownership-tracking
  side of `boundaries.ts` are the natural candidates, so this is likely to be
  someone's problem later.
* **What that future reader must check, so it trips over this rather than
  discovering it:** that `_pendingObserver`'s *value* is not load-bearing across
  two untracked probe reads issued in different orders. If the new reader's
  correctness depends on which observer the field holds, it depends on probe
  ordering, and this entry becomes a bug rather than a labelling question.
* **What would settle it:** whether the order-dependence is intended, or an
  artefact of there being no generation counter to compare against.

### F4 — `scheduler.ts:492`: read-after-replace on the batch

**The one F-item where a wrong answer produces wrong behaviour rather than dead
code.**

```ts
// scheduler.ts:479 — the batch may be replaced here
// scheduler.ts:492
scheduled = dirtyQueue._max >= dirtyQueue._min || this._batch._pendingNodes.length > 0;
```

Line 492 reads `this._batch` *after* line 479 may have swapped in a fresh
`Transition`. A fresh batch has an empty `_pendingNodes`, so the second disjunct
is trivially `false` whenever the replacement happened — and `scheduled` decides
whether another flush is queued.

* **Port's behaviour:** the same order, replace then read. See
  [`src/scheduler.rs:833`](src/scheduler.rs#L833)`-845`.
* **The two readings, indistinguishable from the source:**
  * *intended* — the replacement means the old batch's pending nodes were
    stashed and are no longer this flush's business, so reading the fresh empty
    batch is correct;
  * *a bug* — the read was meant to see the batch as it stood at line 479, and
    replacing first drops a wakeup, stranding work until an unrelated write
    happens to reschedule.
* **Why we cannot settle it here:** no test can supply intent. The two readings
  produce the same result in every case the suite constructs, so the port has no
  evidence either way and would not acquire any by testing harder.
* **What local work has done, since it cannot answer it:** pinned the current
  behaviour, in
  `scheduler::tests::an_incomplete_transition_computes_scheduled_from_the_fresh_batch`.
  A characterisation test — white-box, and deliberately asserting behaviour
  nobody has yet argued is *correct*. It forces `transition_complete()` false by
  setting `actions = 1` directly rather than porting `action.ts` to obtain a
  test, and it fails if the order is inverted. If F4 comes back "bug", the test
  changes with the fix; until then the question cannot be answered by accident.
* **What would still settle it:** upstream.

### G2 — `SPEC-ASYNC-SEMANTICS.md` A19 disagrees with `read()`

**Needs a decision here, not from upstream.** The spec document and the
implementation diverge on A19. Per the phase-2 brief — *"where the spec and the
implementation disagree, the implementation wins"* — the port follows `read()`,
and that part is settled.

What is open is whether to tell upstream, which is an outward-facing call I am
not making unilaterally.

* **Blocked on:** a yes/no, plus a channel — an issue against `solidjs/solid`, a
  comment on an existing thread, or keep it internal.
* **Status:** not started. No issue filed, and none will be without an explicit
  go-ahead.

---

## Outstanding action (not a question)

**B5** — record that arena growth is proportional to nodes *ever created*, not
nodes alive, and make the note findable from the embedding context rather than
only from the README's deviations list.

Flagged here because of *how* it was missed: it is an ask embedded in the prose
of an otherwise-accepted section of `porting-reply.md`, and it does not appear
in that document's own summary list. Still to do.
