-- OTDEL block 1, R05.3 — what a pass spent recovering from a truncated answer.
--
-- Migrations 0001–0010 are applied and frozen; this is additive.
--
-- Why. A live run had the applications pass come back cut off by the model's output limit
-- on its third batch. The scheduler treated that as a provider failure, which failed the
-- whole run — discarding the three passes that had already completed and leaving the job
-- permanently failed with nothing to resume from.
--
-- Truncation is now a recoverable outcome: the same purpose and the same pages are asked
-- again with a larger output envelope, then split in half, and only a single page that
-- still overruns is deferred for that purpose. Every one of those attempts is a real
-- request against a real budget, so it has to be visible.
--
-- Counted apart from `requests_made` (which includes them) on purpose. A pass that spent
-- six of its eight requests recovering is not the same as one that spent six covering the
-- material, and blending them would hide the signal that this purpose's page budget is
-- set too high for this partner's documents.

ALTER TABLE otdel.knowledge_run_passes
    ADD COLUMN truncated_retries integer NOT NULL DEFAULT 0
        CHECK (truncated_retries >= 0);

-- A retry is a request. Recorded retries can never exceed what the pass spent.
ALTER TABLE otdel.knowledge_run_passes
    ADD CONSTRAINT knowledge_run_passes_retries_are_requests
    CHECK (truncated_retries <= requests_made);
