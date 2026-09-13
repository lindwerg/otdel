import { originalMaterialUrl } from '../../api/client'
import type { FactEvidence } from '../../api/types'
import { evidenceSourceLine } from '../../lib/format'

interface EvidenceListProps {
  partnerId: string
  evidence: FactEvidence[]
  /** The model's own wording, when there is any. Never rendered as a quote. */
  modelContext?: string | null
}

/**
 * The proof under a statement: the source's own words, and the way back to the
 * page they came from.
 *
 * Two rules are visible in the markup itself.
 *
 * The quotation is a `<blockquote>` carrying `cite` — it is the fragment the
 * server matched in the stored page text, character for character, not the
 * model's rendering of it. Below it sits the exact source (file and page) as a
 * link that opens the original at that page.
 *
 * Anything the model added is a separate block with its own label, outside the
 * quotation. A reader must never have to guess which half the document said.
 */
export function EvidenceList({ partnerId, evidence, modelContext }: EvidenceListProps) {
  return (
    <div className="evidence">
      {evidence.length === 0 ? (
        // Should not happen — the server refuses a statement without a source —
        // but saying so beats rendering an unsourced claim silently.
        <p className="evidence__empty">Источник не сохранён: утверждение не подтверждено.</p>
      ) : (
        <ul className="evidence__list">
          {evidence.map((item) => (
            <li key={item.id} className="evidence__item">
              <blockquote className="evidence__quote" cite={item.material_filename}>
                <span className="evidence__badge">цитата источника</span>
                <p>{item.quote}</p>
              </blockquote>
              <a
                className="evidence__source"
                href={originalMaterialUrl(partnerId, item.material_id, item.page_number)}
                target="_blank"
                rel="noreferrer"
              >
                {evidenceSourceLine(item.material_filename, item.page_number)}
              </a>
            </li>
          ))}
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
