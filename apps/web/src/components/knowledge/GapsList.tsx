import type { KnowledgeGap } from '../../api/types'
import { questionAudienceLabel } from '../../lib/format'

interface GapsListProps {
  gaps: KnowledgeGap[]
}

/**
 * What the materials do not say.
 *
 * A gap carries no quotation — there is nothing to quote — so it is rendered
 * plainly: what is missing, what that blocks, and the question prepared from
 * it. The question's status is shown as «подготовлен»: phase 1C prepares
 * questions, it does not send them, and the communication channel does not
 * exist yet (`docs/block-01-spec.md` §2). Saying "отправлен" would be the
 * interface claiming an action nobody performed.
 */
export function GapsList({ gaps }: GapsListProps) {
  if (gaps.length === 0) {
    return <p className="knowledge-note">Пробелов пока не зафиксировано.</p>
  }

  return (
    <ul className="gap-list" aria-label="Пробелы и подготовленные вопросы">
      {gaps.map((gap) => (
        <li key={gap.id} className="gap">
          <div className="gap__head">
            <span className="gap__topic">{gap.topic}</span>
            {gap.product_name ? <span className="gap__product">{gap.product_name}</span> : null}
          </div>
          <p className="gap__missing">{gap.missing}</p>
          {gap.blocks ? (
            <p className="gap__blocks">
              <span className="gap__label">блокирует:</span> {gap.blocks}
            </p>
          ) : null}
          {gap.question ? (
            <div className="gap__question">
              <span className="gap__badge">
                {questionAudienceLabel(gap.question.audience)} · подготовлен, канал не подключён
              </span>
              <p>{gap.question.text}</p>
            </div>
          ) : null}
        </li>
      ))}
    </ul>
  )
}
