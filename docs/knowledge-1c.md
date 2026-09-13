# Phase 1C — the product role: what it builds, and what it refuses

Scope: the understanding half of block 1. Pages that phase 1B read become a structured
**draft** of the partner's offering — directions and families, products and services,
characteristics with their units and conditions, a glossary, questions answered from the
material, and the gaps the material leaves. The API is exactly
[`implementation-contract.md`](implementation-contract.md) §"API этапа 1C".

Everything here is a candidate. Verification, publication and search are 1E; industry
research is 1D. Nothing in this phase is published, and nothing answers a customer.

## What is real, and what is waiting

**The model key has not been supplied yet.** That is the expected state, and it is a
state, not a failure:

* the adapter built from a configuration without a key has **no HTTP client inside it**
  (`otdel_llm::build_provider`), so there is no code path from the worker to a network;
* `GET /api/knowledge/provider` reports `needs_configuration` and names the missing
  variables; the interface repeats that and disables the button;
* `POST .../understand` is refused with that same reason instead of queueing work that
  could only fail;
* a run that reaches the worker anyway is recorded as `needs_provider` — **no draft, no
  invented facts, nothing stored**.

Reading materials, page outcomes, tables and source links keep working exactly as before.

**The tests never need a key.** They install a scripted provider
(`otdel_llm::fake::FakeProvider`, behind the `fake` feature) and drive the real prompt
building, the real validation, the real queue and the real database. That is also how
the interesting cases are expressed: a model that cites a source outside the material,
one that invents a quote, one that answers with prose.

## Quick start

```bash
make db-up && make migrate && make bootstrap   # once (adds migration 0004)
make server                                    # API, terminal 1
make worker                                    # extraction + understanding, terminal 2
```

`make worker-probe` now also reports the model adapter:

```
# WARN model adapter not configured: materials are read as usual, and each
#      understanding run is recorded as `needs_provider` without calling anything
#      and without storing invented knowledge
#      state="needs_configuration" missing=["OTDEL_LLM_API_KEY", "OTDEL_LLM_MODEL"]
```

With a key (kept in the git-ignored `.local/otdel.env`, never in the repository):

```bash
OTDEL_LLM_API_KEY=...            # OpenRouter key
OTDEL_LLM_MODEL=openai/gpt-4o-mini
```

## The pipeline

```
material pages (1B)
   │  only `extracted`/`partial` pages that really have text
   ▼
SourceCatalog          labels S1…Sn + a quote index per page
   │
   ├─ plan_batches ──→ bounded prompt ──→ LlmProvider ──→ one JSON object
   │                    (max chars, max pages, max requests per run)
   ▼
validate_response      every candidate re-checked against the same pages
   │
   ▼
replace_draft          one transaction: this material's previous draft out, new in
   │
   ▼
finish_run             status + counters + the reasons for every refusal
```

The model never sees an identifier. It is shown short labels (`S1`, `S2`, …) and must
cite one; the mapping exists only on the server, so a cited label either resolves to a
page of this run or the candidate is refused. There is no third possibility.

## What makes a fact a fact

| Rule | What it prevents | Where |
|---|---|---|
| the cited label resolves in this run's catalogue | a fact attributed to another material, partner or bureau | `otdel-knowledge/src/validate.rs` |
| the quote is found **literally** in that page's stored text | an invented, translated or rounded "quote" | `otdel-knowledge/src/quote.rs` |
| a quote shorter than the floor must be the page's **only** occurrence | a two-character fragment that matches anywhere — while keeping short table values, which are real evidence | same |
| the stored quote is the page's wording, extracted by offset | a citation that drifts from the document | same |
| a fact with no accepted evidence is refused | a claim nobody can check | validate + DB trigger |
| **the value must appear in the quotation, as a whole token** | "нагрузка 10 kN" under a citation that reads 3.5 kN; `5` confirmed by the `5` in `1500` | validate + `quote::contains_token` |
| **every kept citation contains the value**; the others are dropped | a fact showing two genuine quotations, one of which supports a different claim | validate |
| a glossary term is kept only with a citation that uses it | a term illustrated by a fragment that never mentions it | validate |
| the unit must appear in a quotation — never in the model's own value | `3.5` silently becoming `3.5 kN`; `кгс` vouching for itself | validate |
| conditions must appear in a quote, or become model context | an invented "при опирании на две опоры" | validate |
| an unknown product reference refuses the fact | a property attached to the wrong product | validate |
| evidence can only reference a page of its own material | cross-partner citation | composite foreign keys, `0004_knowledge.sql` |
| a fact/term/answer without evidence fails at commit, on INSERT **and UPDATE** | an unsourced row written by any future caller | deferred constraint triggers |

The *attribute* is deliberately not required to be quoted: it is the model's name for
the property ("нагрузка" for a column headed `Load (kN)`), and demanding it literally
would refuse almost every real table. What it labels — value, unit, conditions — must
be in the document, and the quotation is shown beside it so the labelling itself can
be judged.

Matching is done on a normalised copy of both strings — whitespace collapsed, case
folded, dash and quote variants unified — because a text layer and an OCR result
legitimately differ from the visible page in exactly those ways. Latin and Cyrillic
look-alikes are **not** folded: `BC-21` and `ВС-21` are different designations.

Refusals are never silent. Each one increments `facts_rejected` and adds a sentence to
`rejections`, which the interface shows verbatim under the run.

## The model's own words

A model's paraphrase is useful and is kept — in `model_context`, a separate field, shown
under a separate badge («пояснение модели — не цитата»), never inside the quotation.
Three places make the distinction real rather than decorative:

* the validator moves unconfirmed conditions *out* of `conditions` and into
  `model_context`, prefixed with "Условия по формулировке модели";
* a glossary definition marked `definition_from_source` that is not actually in the
  source is re-marked as the model's wording;
* a Q&A answer is marked `answer_is_model_context` unless it is literally in the
  source — which it usually is not, an answer being a synthesis — and the interface
  labels it «ответ сформулирован моделью — не цитата» above the quotation it was
  built from;
* the interface renders a quote as a `<blockquote>` on a raised surface with a gold
  rule, and model context as flat muted text in a dashed box.

## Gaps, not guesses

A missing price, lead time or dimension is a gap: what is missing, what it blocks, and
— when the model proposes one — a question with an addressee. Questions have status
`prepared`: phase 1C prepares them, the communication channel does not exist yet
(`block-01-spec.md` §2), and nothing claims a question was sent. A question without a
stated addressee is not stored at all — guessing would send the wrong question to the
wrong place.

## The queue

`understand_material` is a third job kind. Two things keep it orderly:

* **the two worker halves claim disjoint kinds** (`JobKind::extraction_kinds()` /
  `knowledge_kinds()`), so the document reader can never pick up a knowledge job;
* **the job is keyed by the material**, so pressing "разобрать" twice, or the extraction
  worker finishing twice, reuses one row. A row that is *running* is never re-armed
  (the condition lives in the `ON CONFLICT … WHERE` clause, not between a read and a
  write), and before a worker writes a draft it checks that it still holds the lease —
  a late result of an old run must not replace a newer one (`block-01-spec.md` §7).

Reading a material queues its understanding automatically — the normal path needs no
click (`block-01-spec.md` §11). A material read *before* this phase existed has no run
of its own; the overview lists those separately (`pending_materials`) so the interface
can offer a first draft rather than leaving them invisible. Re-running replaces that material's draft in one
transaction; a **failed** re-run leaves the previous draft in place and says so on the
run, rather than deleting knowledge because a provider blinked.

Failures are classified: a missing key or an unread material is permanent (repeating
cannot help, and a queue that kept retrying would hide the one thing to fix); a
rate-limited or timed-out provider is transient and scheduled again.

A worker killed mid-run would otherwise leave a run saying `running` forever: the
maintenance pass settles such runs (`reclaim_stalled_runs`) once no job stands behind
them, so the interface stops showing work that is not happening.

**After a key is configured**, the runs that stopped as `needs_provider` do not restart
by themselves — the job row is settled, and a worker that silently re-ran everything on
a configuration change would be a surprise. The owner presses «Разобрать заново» (the
button is enabled as soon as the provider reports `ready`), or re-reads the material,
which queues the draft again.

## Limits, cost and logs

Every call is bounded before it is made: input characters, pages per request, requests
per run, output tokens, response bytes, wall-clock timeout and a minimum interval
between requests. A material that does not fit reports the pages it skipped instead of
pretending to have read them.

The key travels only in the `Authorization` header. `ApiKey`, `LlmSettings`, `Config`,
the HTTP client and `AppState` all have hand-written `Debug` implementations that print
`<redacted>`; the request body is asserted in a unit test not to contain the key. Logs
record the model, duration, token counts and response size — never prompt content, never
partner text, never the key.

Budget reservation and spend accounting are **1D** (`block-01-spec.md` §10). This phase
bounds the number of calls; it does not price them.

## Tests

```bash
make check                 # fmt + clippy + everything that needs no database
make test-db               # the full suite, including the database-backed ones
cd apps/web && npm test    # the interface
```

| Suite | What it covers |
|---|---|
| `otdel-core` unit tests | configuration states (no key → what is missing), redaction, the 1C vocabulary |
| `otdel-llm` unit tests | request shape (schema sent, temperature 0, key not in the body), truncated/prose/error responses, the unconfigured adapter reaching no network |
| `otdel-knowledge` unit tests | quote location and refusal, prompt bounds and batching, prompt injection containment, every validation rule |
| `otdel-worker` unit tests | permanent vs transient classification, candidate → storage mapping |
| `otdel-api/tests/knowledge_1c.rs` | upload → read → draft → API, with exact citations; spoofed source; invented quote; no key; cross-bureau access; CSRF; idempotent re-runs; the database refusing an unsourced fact |
| `apps/web` vitest | quotation vs model context, unit only when recorded, the "needs a key" state, refusal reasons shown verbatim |

## Verified against the real catalogues

Run on 2026-09-13 against the two real BASIS documents (32-page catalogue, 12-page
presentation) with OpenRouter and `openai/gpt-4o-mini`, in an isolated scratch database
and scratch storage — the pilot database was not touched. The key came from a
git-ignored env file and appears in no log, response or record.

* extraction first: **44/44 pages read** (32 from the text layer, 12 recognised);
* understanding: **6 model calls**, 8–28 s each, ~50 k characters of source sent;
* stored: 9 directions/families, 44 products, **13 facts, 13 citations**;
* every stored citation was the page text at its own offsets, contained the fact's
  value, and pointed at a page of its own material (checked in SQL and again through
  the HTTP API);
* a deliberately wrong model id produced a real HTTP 400: the run went to `failed`
  with "провайдер ответил ошибкой 400" and **the existing draft was unchanged**.

The run found three defects, all fixed and covered by tests:

1. **References by name.** The model declared products with `ref` and then wrote the
   product's *name* in `product_ref` — because nothing ever explained the convention.
   A whole batch of well-sourced facts was refused. The prompt and the schema now state
   it, and the validator resolves a reference by name as well (ambiguous names resolve
   to nothing).
2. **Short quotations refused by length.** Real table values ("300 мм") were thrown
   away by the minimum-length floor. A short quote is now accepted when it occurs
   exactly once on the page — uniqueness is what the floor was really after.
3. **A citation that supported nothing.** A certificate fact kept two genuine
   quotations, only one of which named that certificate. Citations that do not contain
   the value are now dropped from the fact.

Refusals that remain are the honest kind: a paraphrased quote, a value assembled from
several bullet points, a question with no addressee. They are counted and shown with
their reason under the run.

The 8 skipped pages of the catalogue are the run's own request cap
(`OTDEL_LLM_MAX_REQUESTS_PER_RUN=4` was set for this run to bound spend; the default 8
covers a 32-page document).

## Known limitations

1. **Nothing here is verified.** "Подтверждено цитатой" means the fragment exists in the
   partner's document — not that the document is right, and not that a manufacturer
   confirmed it. The checker is 1E.
2. **Candidates are per material.** The same profile described in two catalogues
   produces two product candidates. Merging identities needs evidence, and
   `block-01-spec.md` §6.6 forbids collapsing synonyms without it; cross-material
   consolidation belongs to a later phase.
3. **No chunking or embeddings.** Semantic search and the embedding profile are 1E; this
   phase stores no vectors, and the plan's "смысловые чанки" item is therefore only
   partly addressed (regions and quotes carry the provenance chain, vectors do not exist
   yet).
4. **Tables are read as text.** Facts are drawn from the page text; a table's cells are
   stored by 1B with their own provenance, but the draft does not yet walk a table
   structurally, so a large load table yields fewer facts than it contains.
5. **Recognised pages inherit OCR errors.** A page read by OCR is offered to the model
   with that stated, and a quote is matched against the recognised text — which can
   itself be wrong. The provenance is exact; the recognition is not.
6. **The response schema is enforced strictly.** An unknown field fails the whole
   response (counted, with the reason). That is deliberate — the schema is sent to the
   provider — but a model that adds a field will produce an empty draft rather than a
   partial one.
7. **One run record per material.** The current state of the draft is kept; the history
   of attempts lives on the job row. There is no timeline of past drafts yet.
8. **Evidence offsets are anchored to the stored page text.** Re-reading a page rewrites
   that text in place, so the `char_start`/`char_end` of an older draft can point at the
   wrong place until the material is drafted again (the quotation itself is stored, so
   it still shows what the document said at the time). Re-reading queues a new draft
   automatically, which repairs it as soon as a provider is configured; nothing yet
   marks the interval as stale.
9. **The yield is low and unmeasured as quality.** 13 facts from 36 read pages is what
   this prompt and this model produced; roughly two thirds of the model's proposals were
   refused. Whether the *accepted* facts are the important ones is a judgement nobody
   has made yet — the checker of 1E is what turns "quoted" into "verified".
10. **Glossary, Q&A and gaps came back empty on the real run.** The model spent its
   answer on products and characteristics. Nothing is wrong mechanically (the synthetic
   suites cover all four), but the prompt does not yet insist on them.
