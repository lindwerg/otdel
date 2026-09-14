# Phase 1F — the cycle around a published version

Scope: what happens *after* something is published. A new document from the partner has to
start a new cycle without touching what is already live; the owner has to be able to see
what is out of date and why, what the new version changed, what happened and in which
order; a published version has to be withdrawable and the withdrawal has to take effect at
once; another agent has to be able to take a complete copy; and the operational history
has to be prunable without ever endangering the knowledge itself. The API is exactly
[`implementation-contract.md`](implementation-contract.md) §"API этапа 1F".

Nothing here is a promise about a product, and nothing here decides what is true. As in
1E, a published version says what documents say, with the fragments that say it.

## What is real, and what is optional

**Nothing in this phase needs anything configured.** The refresh status, the history, the
comparison of two versions, the export and retention are all deterministic reads and
writes over rows that exist. The one place where configuration is visible is the drafting
stage of `POST .../refresh`, and it is *named* rather than skipped: without the 1C model
adapter that step returns `needs_provider` and lists the variables it is missing.

That matters more than it sounds. A pipeline that silently stops at 1C looks exactly like
one that finished — the published version simply never changes — which is the single most
plausible way for this system to quietly lie to its owner.

## Quick start

```bash
make db-up && make migrate && make bootstrap   # once (adds migration 0007)
make server                                    # API, terminal 1
make worker                                    # + the retention sweep, terminal 2
```

Retention is off by default. To switch it on, set both horizons in `.local/otdel.env`:

```bash
OTDEL_RETENTION_EVENT_DAYS=90
OTDEL_RETENTION_JOB_DAYS=30
```

`make worker-once` now also prints `retention_ran`, `events_pruned` and `jobs_pruned`. The
first is separate from the other two on purpose: "did not run" and "ran and removed
nothing" are different answers, and only the second one means the policy is working.

## The chain, completed

```
upload  ──(1A)──►  extract_document
                        │  settle_material: status recomputed from the page rows
                        ▼
                   understand_material            ← chained since 1C
                        │  replace_draft, finish_run
                        ▼
                   validate_partner               ← **chained here, in 1F**
                        │  check → readiness → decide
                        ▼
                   published | blocked            ← every step recorded in otdel.events
```

Until this phase the chain stopped after the draft, and the check was a button. That is
what `block-01-spec.md` §11 forbids — «нормальный путь не требует ручной работы между
этапами» — and it was also the most confusing state the product could be in: new
candidates existed, the published version was unchanged, and nothing said the remaining
step was a click.

Three things keep the new link from being a loop or a surprise:

* a check is queued only when there is something to check. A draft that stored nothing
  leaves the published version alone;
* `validation_pending` means an existing check is joined rather than re-armed, so two
  materials finishing together produce **one** check over both — which is also the only
  way a contradiction between them can be seen;
* a check queues nothing in turn. The chain ends there.

And the check still decides. An unchanged candidate set produces no new version
(`version::decide` → `Unchanged`), so repeated drafting cannot manufacture versions.

## "Is it still current?" — answered without running a check

The refresh status is **computed on every request** and never stored. A stored "stale" flag
is a claim some code has to remember to update, and the first time somebody forgets it is
silently wrong.

Three independent signals feed it, and they are kept apart because they fail differently:

| Signal | What it catches | What it cannot catch |
|---|---|---|
| the 1C run's own status | a drafting run that failed, is queued, is running, or stopped for want of a model | nothing about the document itself |
| per-document state (`content_revision` vs the draft's `source_revision`) | a document re-read after the draft that is in the published version was made | a change to candidates that did not come from a re-read |
| the **candidate fingerprint** | any change to the candidate set: a new fact, a withdrawn one, a different value or quotation | a source that changed *under* an unchanged candidate — the candidate row is identical |
| the last run's `blocked_reasons` / status | a check that ran and published nothing | anything that happened before the last run |

The second row is why there are two fingerprints. 1E's `input_fingerprint` includes each
claim's *verdict*, which is exactly right for "do not publish this twice" and useless here:
computing a verdict requires re-reading every cited source, which is the check itself — the
thing the owner is deciding whether to start. `candidate_fingerprint` covers the candidates
alone, so the comparison is one hex string against another on a plain `GET`. (The rows are
listed above in the order the code consults them, not in the order they are explained.)

The first row is the one that is easiest to get wrong, and this phase did get it wrong
once: the revision a draft used is recorded when the run **starts**, before the model is
called, so a run that then failed leaves a number that looks exactly like success. Reading
only that number reported a document with no facts as «разобран», the partner as `current`,
and answered `up_to_date` to the button meant to fix it. The run's status is therefore part
of the decision and part of the wire (`draft_status`).

The second row is why `content_revision` exists. A changed file is always a *different*
material — deduplication is by content — so the only way the text behind a fixed material
id changes is a re-read. The counter is incremented when a reading **starts**, so a draft
made during a re-read cannot claim the revision that re-read is about to produce; it
carries the older number and is reported as a draft of an older reading, which is the safe
direction.

A version published by phase 1E has no candidate fingerprint at all. That is reported as
`comparison_unavailable` — a comparison that cannot be made — and never as "ничего не
изменилось". Answering a question nobody computed is the failure this field exists to
avoid.

## Reprocess is not retry

| | `POST .../retry` (1A) | `POST .../reprocess` (1F) |
|---|---|---|
| accepts | `failed`, `partial` | `completed`, `partial`, `failed` |
| refuses | `queued`, `processing`, `completed`, `quarantined` | `queued`, `processing`, `quarantined` |
| means | finish work that did not finish | do the work again on purpose |

The second is `block-01-spec.md` §6.1's «явный запуск новой версии обработчика»: a better
OCR engine, a fixed parser, or simply doubt about what was read. `quarantined` is refused
by both, because the file never passed intake and re-reading it would mean opening
something this system declined to open.

Neither touches the stored original, and neither touches a published version: a snapshot
carries **copies** of its quotations, so what was published stays exactly as published no
matter what happens to the document afterwards.

## Comparing two versions

`docs/publication-1e.md` listed the absence of this as known limitation 9. The rules live
in `otdel-publish/src/diff.rs` and are pure functions over two snapshots.

**The hard part is deciding that two claims are the same claim.** `origin_id` cannot do it:
re-running the product role writes new candidate rows with new identifiers, so every claim
of the new version would have an origin nobody has seen, and a re-draft that changed one
number would be reported as "everything removed, everything added". The identity used is
the claim's *subject* — its scope, its product and its property, folded the way search
folds them.

That choice has one consequence and the response states it rather than hiding it: renaming
a property reads as one removal plus one addition. Deciding that «нагрузка» and «предельная
нагрузка» are the same property is a judgement, and 1E already declined to make it when
detecting contradictions (known limitation 2). Making it here would be worse: the output is
a sentence claiming a value *changed*, so a wrong match does not merely omit a caveat, it
invents a change that never happened.

Two more rules worth naming:

* **a verdict that dropped is a change even when the value is identical.** A source that
  moved under an unchanged number reads the same and is no longer supported; a diff that
  compared only values would call that pair identical;
* **a disappearance is not a refutation.** The sentence for a removed claim says the source
  may have changed, been re-read or become unreadable. The system does not know which, and
  says so.

## Withdrawal, and what it takes away

Retraction already took effect immediately in 1E — search resolves the published pointer on
every request — and 1F adds one thing: the version's **chunks** are removed in the same
transaction.

The distinction is deliberate. Claims, citations, gaps and readiness are the snapshot and
are history; the database refuses to delete them and so does this phase. Chunks are the
rebuildable rendering that makes claims findable, plus any vectors computed for them. A
withdrawn version must not be findable, and the pointer protects the *query path* while
this removes the *data* — which is the half a future ANN index or a future query path would
be most likely to reach without asking about status first.

Republishing a retracted version is still impossible: `revoked` is terminal by trigger. A
new check publishes a new version with a new number, which is what leaves the retraction in
the history where it belongs.

## The history, and why it is a table

Every "run" table in this system is one row per scope, rewritten in place. `validation_runs`
has `UNIQUE (partner_id)` and re-arming it zeroes the previous counters; a job's `error` is
overwritten by the next failure; `attempts` is a counter rather than a history. That is the
right shape for *current state* and the wrong shape for «что и почему изменилось».

`otdel.events` is the other shape:

| Property | How it is guaranteed |
|---|---|
| an event is never edited | a trigger refuses `UPDATE` for every writer, including the schema owner; the runtime role has no `UPDATE` grant either |
| an event leaves only through retention | the same trigger refuses `DELETE` unless a transaction-local flag is set, and only `otdel.apply_retention` — `SECURITY DEFINER`, with the floor built in — sets it |
| an event commits with the thing it describes | `events::record` takes the caller's transaction and never opens one. A log that survived a rolled-back upload would describe a material that does not exist |
| an event survives what it is about | `material_id`, `version_id`, `job_id` and `run_id` are **data, not foreign keys**. An audit log that is complete only while nothing has happened is the worst possible one |
| erasing a partner erases their history | the two identifiers that *are* foreign keys are the scope: bureau and partner, both `ON DELETE CASCADE` |
| the kind vocabulary cannot be invented at a call site | a `CHECK` constraint; widening it is a migration |

The sentence shown to the owner is written by the code that knows what happened, not
assembled by the reader from an enum. The structured half (`detail`, `jsonb`, bounded to
4 kB) carries the counters a sentence cannot.

## Retention, and the list it cannot touch

Retention is **off by default**, and that is the design rather than caution: a pilot that
starts deleting its own history because a default said ninety days is a pilot whose first
month has no evidence.

What a sweep may remove: event-log entries past the horizon, and jobs in a terminal state
past theirs — keeping the most recent `keep_per_kind` of each kind per partner regardless
of age, so a partner processed months ago does not end up with an empty processing history.

What it may never remove, whatever the configuration says:

* **published versions and their snapshots** — including superseded and retracted ones.
  `0006_publication.sql` refuses to delete a version that was ever published and this phase
  adds no exception. A retention policy that quietly erased the snapshot somebody's answer
  cited would make every citation in this system conditional;
* **stored originals and their pages** — citations point at them;
* **unfinished jobs** — that is work, not history;
* **the record of the sweep itself** — it is newer than the horizon it just applied.

Three guards make this more than a promise:

1. the runtime role has **no `DELETE` grant** on either table. Removal goes through a
   `SECURITY DEFINER` function;
2. that function refuses a horizon below one day, refuses a job horizon longer than the
   event horizon (the log is what remains after a job row is gone), and refuses to run
   against a bureau other than the caller's own transaction context;
3. the configuration loader refuses the same combinations at **startup**, so a bad policy
   fails when the server starts rather than at the first sweep at three in the morning.

The sweep's own record is written *outside* the deleting transaction. The trigger that
permits a retention delete is transaction-local, and writing the record inside would put
exactly one event under a flag whose whole purpose is to allow removal.

## The export

`GET .../versions/{id}/export` returns one version whole: manifest, header, claims with
their copied quotations, and gaps. The status rules are the same as pinning a version for
search — `published` and `superseded` are readable, `revoked` is refused with its reason,
and `draft`/`validating`/`blocked` are 404 — because exporting an unchecked snapshot would
put candidates into a file that looks exactly like a checked one.

Every export carries `disclosure[]`: what «подтверждено источником» does and does not mean,
that readiness authorises nothing, that absent commercial terms are not filled in, and that
this is a snapshot of one version which may since have been replaced. An export is the one
artefact that leaves every screen that would otherwise say those things.

Reading an export appends an `export_read` event. That makes a `GET` write a row, which is
worth stating plainly rather than hiding: the alternative is a system that cannot answer
"who took a copy of the withdrawn version, and when".

## What the interface must not do, and does not

* **no invented progress.** There is no bar, no percentage and no estimate anywhere in this
  phase. While a check is running the state says so and the page polls; the refresh plan
  reports how many steps were *queued*, which is a count of rows, not a measure of
  completion;
* **every reason names its source.** A reason about a document carries the file name and
  both reading numbers;
* **refusals are as loud as acceptances.** A stage that needs configuration is shown with
  the variables it is missing;
* **the server's words win.** `message`, `summary`, `limitations[]`, `protected[]` and
  `disclosure[]` are rendered verbatim; the components add a title and nothing else;
* **a failed refresh does not clear the screen.** What is shown stays what the server last
  said, with the error above it.

## Tests

```bash
make check                 # fmt + clippy + everything that needs no database
make test-db               # the full suite, including the database-backed ones
cd apps/web && npm test    # the interface
```

| Suite | What it covers |
|---|---|
| `otdel-core` unit tests | the 1F vocabulary and its database spellings, the retention configuration states (default keeps everything; a horizon below the floor is refused; a job horizon longer than the log's is refused), the export disclosure, the reprocess/retry distinction |
| `otdel-publish/src/diff.rs` unit tests | a re-draft with new identifiers read as a change rather than a replacement; a dropped verdict with an identical value; a renamed property as an addition plus a removal with the limitation stated; a disappearance not called a refutation; sources compared order-independently; an industry claim never matching a partner claim |
| `otdel-publish/src/version.rs` unit tests | the candidate fingerprint: order-independent, sensitive to a new candidate and to a changed value, and distinct from the verdict-sensitive one |
| `otdel-db` unit tests | the event summary clipped by characters rather than bytes, a non-object payload wrapped rather than dropped, the empty retention outcome |
| `otdel-api/tests/updates_1f.rs` | the nineteen acceptance scenarios of [`uat-1f.md`](uat-1f.md), including the maintenance pass that actually runs the sweep |
| `apps/web` vitest | no percentage or progress bar for server work; a reason naming its document and both readings; an unconfigured stage reported rather than hidden; the server's refusal shown instead of an invented one; the history offering no way to edit itself; retention stating what it protects; export and comparison disabled when there is nothing to export or compare |

## Verified

Run on 2026-09-13 against the project's PostgreSQL 17 (pgvector image), in the separate
`otdel_test` database — the pilot database's rows were not touched.

* migration `0007_updates.sql` applies cleanly on a database that already carries
  0001–0006, and `make migrate` is idempotent across it;
* 19 database-backed 1F tests pass, and so do the 1A–1E suites unchanged except for two
  assertions that count `Material`'s fields (`content_revision` is the twelfth);
* `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings`
  are clean; `cargo test --workspace` passes;
* `apps/web`: `tsc -b`, `oxlint`, `vitest run` (126 tests) and `vite build` all pass;
* **no test opened a socket.** The model adapter is the scripted one; the embedding and
  search adapters are the unconfigured ones, which have no HTTP client to use.

Defects found and fixed during the work:

1. **A second draft of the same material stored nothing.** The understanding queue is
   idempotent per material, so a test that drafted twice silently drafted once. The fix is
   in the test harness rather than the product — the queue is behaving correctly — but it
   is worth recording, because the same shape would confuse an operator re-drafting by
   hand: the second press joins the first run, it does not start a second.
2. **The candidate fingerprint had to be a second column, not a reinterpretation of the
   first.** The first attempt compared today's candidates against `input_fingerprint`,
   which always differs because that hash includes verdicts — so every partner would
   permanently read as "needs revalidation".
3. **The retention record could not be written inside the sweep.** The trigger that permits
   a retention delete is transaction-local; the sweep's own event would have been created
   under it. It is written in its own transaction afterwards.
4. **A single-page re-read did not count as a reading.** `content_revision` was incremented
   only on the whole-document path, so retrying one `partial` page rewrote that page's text
   while the refresh status went on calling the document `drafted` — up to date — with the
   citation behind its facts moved underneath. A page re-read now counts as a reading of
   the document, which is the true statement: the draft was made from a material, and that
   material's text is not what it was.
5. **The comparison silently dropped every claim but one per subject.** Grouping by
   `(scope, product, attribute)` into a map kept the *last* claim of each group, and two
   claims can share a subject — that is exactly what `mark_contradictions` detects. A
   version holding two disagreeing loads for one profile was compared by whichever sorted
   last, so a claim that really disappeared was reported as a value change that never
   happened, and the counters stopped summing to the number of claims. Groups are lists
   now, paired explicitly: identical claims first, the rest positionally, leftovers as
   additions and removals.
6. **A drafting run that failed was reported as a finished draft.** See "Is it still
   current?" above. It also made `POST /refresh` answer `up_to_date`, so the screen that
   exists to unstick the partner had no effect at all.
7. **A draft finishing while a check was already *running* was dropped.** The check reads
   its candidates once, in its first transaction; `queue_check` saw a check "pending" and
   returned. Uploading three documents at once was enough — the first draft started a
   check, the other two committed while it worked, and their facts would never have been
   checked or published, with nothing queued to fix it. The checker now compares what it
   read with what exists when it finishes and queues one more if they differ; publishing
   does not move the candidate set, so this cannot loop.
8. **`POST /refresh` could 500 and roll back everything it had just queued.** Its event
   embedded one object per step and the payload column is bounded at 4 kB, so a partner
   with enough documents overflowed the CHECK *after* the enqueues and before the commit —
   every time, with no way to make progress. The payload is a fixed-size summary now.
9. **The interface turned "no sweep ever removed anything" into "очистка ещё ни разу не
   выполнялась".** A sweep records itself only when it removed something, which with a
   configured policy is most of the time. The three cases are now distinguished.
10. **A version that was never published was rendered as if it were live.** The panel
    falls back to the newest version when nothing is published, and the comparison did not
    show the status. It does now, with a line saying search does not use it.
11. **The screen went stale after a refresh.** Polling was armed from `checking` alone, and
    a refresh most often queues reading and drafting and no check. It now polls while the
    server says any of the three is unfinished.
12. **The refresh status loaded every page of every document on every poll.** It reused the
   checker's candidate loader, which carries the full source text because re-reading it is
   what a check *is*. The interface polls this endpoint every four seconds while a check
   runs, so that was megabytes per poll to hash a few hundred bytes. A digest loader that
   selects the values and the quotations and nothing else now serves the request path, and
   a test asserts that both loaders hash to the same string — if they ever disagreed, every
   partner would permanently read as "нужна перепроверка".

## Known limitations

1. **The comparison matches on the property name.** A renamed property is one removal and
   one addition. Stated in `limitations[]` on every response.
2. **A source that changed under an unchanged candidate does not move the candidate
   fingerprint.** That case is covered by the source-revision comparison instead, and only
   when the change came from a re-read *this system performed* — a whole document or a
   single page. A page edited in the database by hand is caught by the checker at the next
   run, not by the refresh status.
3. **Retraction is still all or nothing.** A version is withdrawn whole. Withdrawing a
   single wrong fact while keeping the rest — and the per-claim dependency tracking that
   would need — is not implemented, and `docs/publication-1e.md`'s limitation 10 stands.
4. **The actor is a role, not a person.** There is one human account in the local pilot, so
   every owner action is recorded as `owner`. A per-user identity belongs with the accounts
   that would need it.
5. **The export is session-authenticated.** A downstream agent uses the same session as the
   owner. A scoped, revocable machine credential is a separate piece of work and inventing
   one here would have meant inventing a secret mechanism this phase was told not to touch.
6. **Retention has no per-partner policy.** One bureau, one pair of horizons.
7. **The history is not searchable.** It is paged by time (`limit`, `before`) and nothing
   more. Filtering by kind or by material would be easy and is not there, because nothing
   in the acceptance run needed it.
8. **`job_failed` is written for the check only.** A failed `validate_partner` job records
   one, because that is the failure which answers «почему версия прежняя». Failures of the
   reading and drafting halves stay on the job row (`attempts`, `error`, `error_kind`) and
   in the run they belong to; recording one event per failed attempt of a retried document
   would duplicate the queue in the log.
9. **No external provider is integrated, still.** Everything in this phase is deterministic
   and needs none; the drafting stage that does need one reports what is missing. "Работает
   с настоящей моделью" is not a claim this phase makes either.
