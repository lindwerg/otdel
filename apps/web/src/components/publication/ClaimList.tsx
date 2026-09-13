import { originalMaterialUrl } from '../../api/client'
import type { VersionClaim, VersionEvidence } from '../../api/types'
import {
  claimStatusPresentation,
  claimValueLine,
  evidenceSourceLine,
  formatDateTime,
  isClaimSourceSupported,
} from '../../lib/format'

interface VersionEvidenceListProps {
  partnerId: string
  evidence: VersionEvidence[]
  /** The model's own wording, when there is any. Never rendered as a quote. */
  modelContext?: string | null
}

/**
 * Citations copied into the version, in the `.evidence*` markup the 1C and 1D
 * screens already use.
 *
 * Reusing that markup is the point: a reader who learned once that a bordered
 * `<blockquote>` under a gold badge is the source's own words should not have to
 * learn it again here. Two source kinds share it. A `material` citation links
 * back into the partner's own file at the exact page. An `external` one links out
 * — with `rel="noreferrer nofollow"`, because an untrusted page must receive
 * neither our referrer nor our link equity — and carries the date it was read,
 * since an external page can change after the fact and the quote here cannot.
 *
 * `model_context` is rendered outside the quotation under its own badge. A
 * reader must never have to guess which half a source actually said.
 */
export function VersionEvidenceList({
  partnerId,
  evidence,
  modelContext,
}: VersionEvidenceListProps) {
  return (
    <div className="evidence">
      {evidence.length === 0 ? (
        // Should not happen — a deferred database trigger refuses a claim without
        // evidence — but saying so beats rendering an unsourced claim silently.
        <p className="evidence__empty">Цитата не сохранена: утверждение не подтверждено.</p>
      ) : (
        <ul className="evidence__list">
          {evidence.map((item) =>
            item.source_kind === 'material' ? (
              <li key={item.id} className="evidence__item" data-kind="material">
                <blockquote
                  className="evidence__quote"
                  cite={item.material_filename ?? undefined}
                >
                  <span className="evidence__badge">цитата материала партнёра</span>
                  <p>{item.quote}</p>
                </blockquote>
                {item.material_id ? (
                  <a
                    className="evidence__source"
                    href={originalMaterialUrl(
                      partnerId,
                      item.material_id,
                      item.page_number ?? undefined,
                    )}
                    target="_blank"
                    rel="noreferrer"
                  >
                    {evidenceSourceLine(
                      item.material_filename ?? 'материал партнёра',
                      item.page_number ?? 0,
                    )}
                  </a>
                ) : (
                  <p className="evidence__meta">материал не указан в снимке версии</p>
                )}
              </li>
            ) : (
              <li key={item.id} className="evidence__item" data-kind="external">
                <blockquote className="evidence__quote" cite={item.url ?? undefined}>
                  <span className="evidence__badge">цитата внешнего источника</span>
                  <p>{item.quote}</p>
                </blockquote>
                {item.url ? (
                  <a
                    className="evidence__source"
                    href={item.url}
                    target="_blank"
                    rel="noreferrer nofollow"
                  >
                    {item.host ?? item.url}
                  </a>
                ) : null}
                <p className="evidence__meta">
                  {item.retrieved_at
                    ? `прочитано ${formatDateTime(item.retrieved_at)}`
                    : 'дата чтения не сохранена'}
                  {' · цитата скопирована в версию и с тех пор не перечитывалась'}
                </p>
              </li>
            ),
          )}
        </ul>
      )}

      {modelContext ? (
        <p className="evidence__context">
          <span className="evidence__badge evidence__badge--model">
            пояснение модели — не цитата
          </span>
          {modelContext}
        </p>
      ) : null}
    </div>
  )
}

interface ClaimListProps {
  partnerId: string
  claims: VersionClaim[]
  /** Russian list label; the surrounding screen says what this set of claims is. */
  label: string
  emptyNote?: string
}

/**
 * Statements that belong to a **version** — never a 1C/1D candidate.
 *
 * Every entry leads with its verdict, and the verdict is the thing the layout
 * protects. Only `source_supported` is allowed to look confirmed, and even it is
 * worded as «подтверждено источником» with the caveat attached: the cited source
 * says so, which is not an independent verification and not a guarantee of
 * truth. A reader who takes «подтверждено» to mean "somebody checked this" has
 * been misled by the interface, not by the data.
 *
 * Everything else carries an explicit «источником не подтверждено» marker in
 * addition to its own verdict, because `hypothesis`, `unknown`, `conflicted` and
 * `stale` differ in *why* they are unconfirmed but not in *whether* they are.
 * None of them is styled as an error: a recorded hypothesis is the check working.
 *
 * `check_note` is printed verbatim under the verdict. It is the server's own
 * explanation of why this status and not another one, and it is the only text
 * here that is specific to this particular claim.
 */
export function ClaimList({ partnerId, claims, label, emptyNote }: ClaimListProps) {
  if (claims.length === 0) {
    return (
      <p className="knowledge-note">
        {emptyNote ?? 'В этой версии нет утверждений по этому запросу.'}
      </p>
    )
  }

  return (
    <ul className="claim-list" aria-label={label}>
      {claims.map((claim) => {
        const presentation = claimStatusPresentation(claim.status)
        const supported = isClaimSourceSupported(claim.status)
        return (
          <li key={claim.id} className="claim" data-status={claim.status} data-scope={claim.scope}>
            <div className="claim__head">
              <span className="claim__verdict" data-tone={presentation.tone}>
                {presentation.label}
              </span>
              {supported ? null : (
                <span className="claim__unconfirmed">не подтверждено источником</span>
              )}
              {claim.scope === 'industry' ? (
                <span className="claim__scope">отраслевое — не о продукции партнёра</span>
              ) : null}
              {claim.product_name ? (
                <span className="claim__product">{claim.product_name}</span>
              ) : null}
            </div>

            <p className="claim__statement">
              <span className="claim__attribute">{claim.attribute}:</span>{' '}
              <span className="claim__value">{claimValueLine(claim)}</span>
            </p>

            {claim.conditions ? (
              <p className="claim__conditions">
                <span className="claim__label">условия:</span> {claim.conditions}
              </p>
            ) : null}

            <p className="claim__hint">{presentation.defaultHint}</p>

            {claim.check_note ? (
              <p className="claim__note">
                <span className="claim__label">заключение проверки:</span> {claim.check_note}
              </p>
            ) : null}

            <VersionEvidenceList
              partnerId={partnerId}
              evidence={claim.evidence}
              modelContext={claim.model_context}
            />
          </li>
        )
      })}
    </ul>
  )
}
