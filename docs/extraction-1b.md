# Phase 1B — reading documents: running it, and what it actually does

Scope: the reading half of block 1. A material that intake stored is read page by page;
each page gets an explicit outcome, its text, its structural regions and — where a grid
is really there — its tables. The API is exactly
[`implementation-contract.md`](implementation-contract.md) §"API этапа 1B".

**What is real and what is not.** The text layer of a PDF is read in process, in pure
Rust, with no external program involved. Recognition (OCR) is *not* implemented here: the
worker drives local binaries — `tesseract` and `pdftoppm` — through adapters that probe
for them. On a machine without those binaries no page is ever recognised, and the pages
that would have needed recognition are recorded as `needs_ocr` **with the reason**. They
are not reported as read, not reported as empty, and never given invented text.

## Quick start

```bash
make db-up && make migrate && make bootstrap   # once (adds migration 0003)
make server                                    # API, terminal 1
make worker                                    # extraction + maintenance, terminal 2
```

Check what recognition is available before a long run:

```bash
make worker-probe
# OCR engine available  engine="tesseract 5.5.3"
# page rasteriser available  rasteriser="pdftoppm version 26.05.0"
```

or, when it is not installed:

```
# WARN OCR engine unavailable: pages without a usable text layer will be recorded
#      as `needs_ocr` with this reason, not as read
#      reason="исполняемый файл `tesseract` не найден"
```

`make worker-once` does a single pass and prints counters — that is the form the
acceptance checks use.

## Optional dependencies

| Tool | Used for | Without it |
|---|---|---|
| `tesseract` (+ language packs) | recognising a rendered page | pages that need it are `needs_ocr` with the reason |
| `pdftoppm` (poppler) | rendering one PDF page to an image | the same, naming the rasteriser instead |

```bash
brew install tesseract tesseract-lang poppler      # macOS
apt install tesseract-ocr tesseract-ocr-rus poppler-utils  # Debian/Ubuntu
```

Both are named by configuration (`OTDEL_OCR_ENGINE_BIN`, `OTDEL_OCR_RENDERER_BIN`), never
by a document. A bare name is resolved through `PATH`; anything containing a separator
must be an absolute path, and a leading `-` is refused. Every argument the worker passes
is a fixed flag, an integer it computed, or a path it created — no file name and no text
from inside a PDF ever reaches a command line, and no shell is involved.

**The tests do not need either tool.** The suites build PDFs in memory and, where a
working engine is required, install a stub. They therefore produce the same result on a
machine with Tesseract and on one without.

## How a page is decided

```
text layer → assessment ──usable──────────────→ extracted (text_layer)
                 │
                 ├─unusable─→ recognition ──ok──→ extracted (ocr)
                 │                 └──unavailable/empty──→ needs_ocr + reason
                 ├─thin─────→ recognition ──ok──→ extracted (ocr)
                 │                 └──unavailable──→ partial  (the thin text is kept)
                 └─blank────────────────────────→ empty
```

The whole rule lives in `crates/otdel-extract/src/assess.rs`, in two pure functions, and
is covered by unit tests. Three judgements are worth stating:

* a page with **no text layer but with images or drawings** is never `empty`. `empty` is
  reserved for a page that carries nothing at all;
* a text layer that decodes to **replacement characters** is treated as absent. Storing
  it would produce confident nonsense;
* a page cut short by the per-page glyph budget is `partial`, not `extracted`.

The material status is *derived* from the page outcomes and never set on its own:
every page settled cleanly → `completed`; nothing usable at all → `failed`; anything in
between → `partial`. One failed page therefore prevents `completed` — which is the
point (`block-01-spec.md` §6.2).

## Tables

PDF catalogues draw tables as text at coordinates; there is no table object to read. The
columns are recovered from a vertical corridor of whitespace that **no row of the
candidate block crosses**, and at least three consecutive rows and two columns are
required. When that test fails no table is produced and the lines stay paragraphs — a
missing table is a visible gap, a wrong one silently corrupts values.

What is stored per cell is the verbatim fragment (`raw_text`) plus a classification
(`number` / `text` / `empty`). **There is no numeric column by design.** A blank cell
stays blank instead of becoming a zero, and a range or a designation (`40…60`,
`BP 21/21D`, `– / 2074 / 2345`) stays text. A `unit` is recorded only when it is literally
written in the cell or in that column's header, and the header itself is kept verbatim in
`column_header`.

## Retry

Two levels, both idempotent:

* **whole material** — `POST .../materials/{id}/retry`, allowed for `failed`/`partial`.
  The pages are updated in place rather than deleted and recreated, so identifiers
  recorded elsewhere stay valid and an interrupted retry never leaves a material with no
  pages at all;
* **one page** — `POST .../pages/{n}/retry`, allowed for `pending`, `needs_ocr`,
  `partial` and `failed`. The queue row is keyed by `(material, page)`, so pressing the
  button twice does not create a second job.

`attempts` on a page counts how many times it really was read; it is not reset by a retry.

## The queue

A job is claimed in a short transaction with `FOR UPDATE SKIP LOCKED`; the document is
read **outside** any transaction, with the lease renewed once per page. If the lease is
lost mid-run the worker stops immediately rather than writing results another worker may
already be replacing. An expired lease is returned to the queue by the maintenance pass,
so an interrupted run resumes without a human.

Failures are classified: *permanent* (the file is not a PDF, the document is encrypted,
it exceeds the page limit) stops immediately with the reason; *transient* (storage
blinked) is scheduled again until the attempt limit.

## Source links

A stored region carries `bbox` in PDF user space (origin bottom-left, points) when the
adapter could place it, and `null` when it could not — recognised text has no coordinates
and none are invented. The interface links to the original with the standard `#page=N`
fragment.

**Phase 1B stores no page images.** A page is rendered only for the duration of
recognition, into a private temporary directory that is deleted when the job ends, and it
is never published through the API.

## Tests

```bash
make check                 # fmt + clippy + everything that needs no database
make test-db               # the full suite, including the database-backed ones
```

| Suite | What it covers |
|---|---|
| `otdel-extract` unit tests | the decision rule, units and cell classification, layout, table recovery |
| `otdel-extract/tests/reading_pdfs.rs` | reading real (synthetic) PDFs end to end, with and without a text layer |
| `otdel-api/tests/extraction_1b.rs` | upload → worker → page records → API, including isolation and retry |
| `otdel-api/tests/extraction_queue.rs` | leasing, lease loss, permanent vs transient failure, lease recovery |
| `apps/web` vitest | honest page labels, no invented percentages, table rendering with blank cells |

## Verified against the real catalogues

The two real BASIS documents are not in this repository. They were run through the real
binaries (`otdel-api serve`, `otdel-worker once`) against a scratch storage directory and
the test database; the pilot database and its storage were not touched.

With `tesseract 5.5.3` and `pdftoppm 26.05.0` installed:

* **catalogue, 32 pages** → `completed`, 32 read: 29 from the text layer (Russian decoded
  correctly), 3 recognised. Tables recovered across the product pages — page 7 alone
  yields five, including the BP 21 load tables whose `– / 2074 / 2345` triplets are kept
  verbatim as text rather than turned into numbers;
* **presentation, 12 pages** → `completed`, all 12 recognised, `ocr_engine = tesseract`.

With the binaries deliberately absent:

* **presentation** → `failed`, `pages_total = 12`, `pages_needs_ocr = 12`,
  `ocr_engine = null`, no text stored, and every page carrying a reason naming the
  missing executable.

That exercise found **two real defects** the synthetic fixtures could not:

1. a row of dash-separated load values was promoted to a header row and attached as
   `column_header` to every value below it. A header now requires that no cell reads as a
   number and that the cells read like words;
2. the catalogue's font names its glyphs `uniXXXX`, a form the parser's table does not
   know, so those characters decoded to `U+0000` — which PostgreSQL refuses in a `text`
   column, failing the whole 32-page job with a database error. Undecodable control
   characters are now dropped from *stored* text (while still counting towards the
   garbled ratio that sends a page to recognition), and the repository layer enforces the
   same at the boundary so no caller can get it wrong.

## Known limitations

1. **Recognition quality is the engine's.** OTDEL records which engine and language pack
   produced a page and how long it took; it does not score the result, and there is no
   per-page confidence value — reporting one would suggest a measurement that has not
   been made.
2. **No orientation detection.** The engine runs with `--psm 3`; a sideways scan may
   recognise poorly. `/Rotate` from the PDF is recorded on the page but the render is not
   rotated to compensate.
3. **Line grouping is page-wide.** A genuinely two-column page interleaves its columns in
   the plain text. Regions keep correct coordinates and tables are recovered separately,
   but the reading order of a magazine-style layout is not reliable.
4. **Recognised text has no coordinates.** A region from OCR is stored with `bbox = null`.
5. **Region kinds are advisory.** `heading` / `footnote` are geometric guesses; the text
   itself is exact. On pages where drawing labels share a baseline with body text the
   grouping is noisy.
6. **Images are recognised, not described.** A material that is itself a PNG/JPEG becomes
   one page and goes straight to recognition; a diagram inside a PDF is counted, not
   interpreted. Vision models are a later phase.
7. **No incremental re-read.** Re-reading a material re-reads every page; only the
   single-page retry is selective.
