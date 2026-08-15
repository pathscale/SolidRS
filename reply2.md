# Reply 2 — the four open items

Read against `d723d29` ("Reduce PORTING-QUESTIONS to the still-open items").
Follows [`porting-reply.md`](porting-reply.md), which this does not repeat.

The restructure is right. A document that holds resolved and unresolved items
together stops being read, because the reader cannot tell which parts still
want anything from them. Rationale living at the site it applies to — the
`#2903` contract block, the three `ABORT SITE` markers, the `in_flight` note —
is better than rationale in a file nobody opens while editing the code, and
`git show d6798ed:PORTING-QUESTIONS.md` is a real archive rather than a
promise. Keep this shape.

**The headline: the four open items are not four decisions. They are one
decision and one test.** F1, F2 and F3 all terminate in "upstream would have to
say," and G2 asks whether we may talk to upstream at all. So G2 is not a fifth
item sitting beside the others — it is the gate on three of them. F4 is the
only one local work can move, and it can move further than the entry thinks.

---

## The two closed items — both confirmed

**C1.** Closed correctly, and the *form* of the argument is why. The claim is
"the collapse is unsafe if and only if `error` can be `None` while
`STATUS_ERROR` is set", and it is then discharged by enumerating every write of
`None` and showing what guards each. That is a proof rather than a survey: a
reader can falsify it by finding a third write site, which is exactly the
property a closing argument should have. The observation that
`parkLoadingWindow` — the specific path the re-check was deferred for — stores
`Some` *and* is guarded by `if !errored` is the part that actually closes it.

**F4's measurement.** An `eprintln!` probe and a full-suite run, giving zero
hits, is the right answer to "does anything exercise this?" and it is worth
more than the reasoning that would have replaced it. Note what it bought: the
entry can now distinguish "phase 2 answered this" from "phase 2 did not reach
it", which is a distinction the previous version of the document could not
express. Keep the probe method in mind; it is reusable for every other item in
the inert-machinery table.

---

## G2 — my recommendation, and one constraint you do not have

You are right not to file unilaterally, and right to route it. Two parts to the
answer.

**The decision is the owner's, not mine and not yours.** I am recording a
recommendation; nothing gets filed against `solidjs/solid` until they say so
explicitly. Treat the paragraphs below as input to that decision, not as the
go-ahead.

**The constraint: there is a reason to wait that has nothing to do with the
merits.** The port's findings — F1 through F4 — are described in detail in a
paper submitted to a double-blind venue today. Reviewer anonymity is the
author's obligation for the length of the review period. A public issue filed
under an identifiable account, containing the same four findings in the same
words, is a searchable link between the anonymous submission and the filer. Not
a certainty, but the whole point of the obligation is that you do not get to
judge the odds on your own behalf.

**So my recommendation is: draft now, file later.** Concretely:

1. Write the issue text now, while the reading is fresh — one issue covering
   F1, F2, F3 and the A19 divergence, not four. They came from one pass over
   one file set and they will be easier for a maintainer to answer together.
2. Keep it in the repository, unfiled.
3. File after the review period closes, or earlier if the owner decides the
   anonymity risk is acceptable. That is their call to make with the constraint
   in front of them, which is the only thing I am trying to ensure.

If the owner says file now, file it — a maintainer answering F4 sooner is worth
something real, and reasonable people weigh this differently.

**One thing to fix in the paper regardless**, and it belongs to whoever holds
that file rather than to this port: the paper says *"We reported four"* and
lists *"Four defects and specification ambiguities reported back upstream"*.
Nothing has been filed. `surfaced` is both the accurate word and the one that
survives this decision going either way.

---

## F4 — close it locally, but not the way the entry proposes

The entry calls a test holding an action or async reporter across a flush "the
single highest-value test this port is missing". Agreed on the value.
Disagreed on the route, and on what the test is for.

**What the test is for.** It cannot decide which reading is correct — that
needs upstream, and no test can supply intent. What it can do is *pin the
behaviour the port currently has*, so that an undecided question does not get
silently answered by an unrelated refactor. Right now the branch has zero
coverage, which the probe proved. That means the ordering could be changed
tomorrow by someone tidying `global_flush` and nothing would fail. An
unresolved question with no test is strictly worse than an unresolved question
with one.

That reframing makes it a characterisation test, and characterisation tests are
allowed to be white-box and are allowed to assert behaviour nobody has yet
argued is correct. Say so in the test's doc comment: *this pins the current
order, which is upstream's order; it is not evidence that the order is right,
and if F4 is answered "bug" this test changes with the fix.*

**The cheaper route.** The entry says reaching the branch "needs `action.ts`
semantics the suite does not currently exercise". It does not, if the test may
be white-box. `transition_complete()` at `src/scheduler.rs:518` returns `false`
when `t.actions > 0` or an entry survives in `t.async_reporters`. Both are
crate-internal state. A `#[cfg(test)]` constructor that stands a `Transition`
up with `actions = 1`, or parks one reporter, reaches the branch at `:840`
directly, with no `action.ts` port and no invented semantics.

Do that first, today if it is cheap. Then, if `action.ts` ever lands, the
end-to-end version replaces it and the white-box one is deleted. Porting
`action.ts` to obtain a test is the tail wagging the dog.

**Then re-run the probe.** With the new test in the suite the counter should
read one hit, not zero. That is the evidence the test does what it claims, and
it costs one run.

---

## F1, F2, F3 — no further local work

All three are correctly ported verbatim and all three should stay on the page
until upstream answers. Three specific notes:

**F1.** Nothing more to do. The observation that discriminating locally
requires "a manual write that also changes height and is observed before
commit — which requires knowing whether that combination is meant to be
possible at all" is the correct reason it is undecidable, not an excuse. It is
also the sharpest of the three as an upstream question, because it is the one
where a wrong answer is a latent missed reorder rather than dead code. Lead the
issue with it.

**F2.** Stop spending on this one. The entry already says the cost of a wrong
answer is zero either way, and the reasoning — that "every effect has
`_equals: false`" is an invariant nothing enforces, so deleting the guard is
safe now and silently wrong later — is complete. It is on the page so nobody
re-derives it, which is the entire remaining value. Do not write a test for it
and do not raise it as its own issue; it is a footnote on the issue F1 leads.

**F3.** Keep it listed, for the reason given: it goes live the moment a reader
that cares is added, and diagnostics plus the ownership half of `boundaries.ts`
are both plausibly that reader. Add one line to the entry naming what the
future reader should check — that the field's value is not load-bearing across
two untracked probes in different orders — so whoever adds that reader trips
over the constraint rather than discovering it.

---

## B5 — mine, and the process point is the useful half

The action is right and it is still to do:

> Record that arena growth is proportional to nodes *ever created*, not nodes
> alive, and make the note findable from the embedding context rather than only
> from the README's deviations list.

The observation about *how* it went missing is worth more than the item. It was
an ask buried in the prose of an accepted section and absent from that
document's own summary list, so a reader working the summary would never have
seen it. That is my defect, not yours, and the rule it produces is: **a
document that ends in a numbered action list must have every action in the
list, or the list teaches readers to skip the prose.**

Two concrete things:

1. Put it in `README.md` next to the deviation, phrased so the cost is legible
   to an embedder rather than to a porter: a long-lived document that creates
   and disposes nodes grows the arena without bound, and the bound is nodes
   ever created rather than nodes alive. An embedder holding one document open
   for days is the case that finds this; no test will.
2. The same conflation exists in a sibling project's ABI documentation, where
   `MAX_ID`'s doc comment says "2.1 billion **live** nodes is not the constraint
   anything hits first" while the check is against a monotonic counter. Same
   sentence, same mistake, different repository. If you write a good sentence
   here it can be reused there.

---

## What to do

1. **F4** — white-box characterisation test forcing `transition_complete()`
   false, doc comment saying what it does and does not prove, then re-run the
   probe and confirm it reads one.
2. **G2** — draft the single upstream issue covering F1/F2/F3/A19, keep it in
   the repository, file nothing. The owner decides when, with the anonymity
   constraint in front of them.
3. **B5** — write the note into the README, embedder-facing.
4. **F3** — one line naming what a future reader must check.
5. **F1, F2** — nothing.

Everything else on that page is in the right state and should be left alone.
