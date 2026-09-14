import type { KnowledgeUncertainty } from '../../api/types'
import { uncertaintyKindLabel } from '../../lib/passport'

interface UncertaintyListProps {
  uncertainties: KnowledgeUncertainty[]
}

/**
 * What the material says and nobody may read as a value.
 *
 * Kept visually apart from facts, and deliberately so. A gap records what the
 * document does *not* say; an uncertainty records what it *does* say in a form
 * that cannot be trusted — an unreadable page, a load column with no unit, a
 * number whose product cannot be determined. Both are answers, and neither is
 * ever a fact.
 *
 * The cell's own text is shown when there is one, under a label saying it is not
 * a claim. Hiding it would leave the reader unable to see the problem; showing
 * it as a quotation beside facts would make it look like one.
 */
export function UncertaintyList({ uncertainties }: UncertaintyListProps) {
  if (uncertainties.length === 0) {
    return <p className="knowledge-note">Неясностей не зафиксировано.</p>
  }

  return (
    <ul className="uncertainties" aria-label="Что нельзя прочитать однозначно">
      {uncertainties.map((item) => (
        <li key={item.id} className={`uncertainty uncertainty--${item.kind}`}>
          <div className="uncertainty__head">
            <span className="uncertainty__kind">{uncertaintyKindLabel(item.kind)}</span>
            {item.page_number != null ? (
              <span className="uncertainty__page">с. {item.page_number}</span>
            ) : null}
          </div>
          <p className="uncertainty__subject">{item.subject}</p>
          <p className="uncertainty__detail">{item.detail}</p>

          {item.quote ? (
            <p className="uncertainty__quote">
              <span className="evidence__badge evidence__badge--model">
                текст из источника — читать как значение нельзя
              </span>
              {item.quote}
            </p>
          ) : null}

          {item.reasons.length > 0 ? (
            <ul className="uncertainty__reasons">
              {item.reasons.map((reason) => (
                <li key={reason}>
                  <code>{reason}</code>
                </li>
              ))}
            </ul>
          ) : null}
        </li>
      ))}
    </ul>
  )
}
