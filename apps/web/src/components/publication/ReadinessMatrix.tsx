import type { ReadinessEntry, VersionGap } from '../../api/types'
import { READINESS_TOPICS, readinessStatePresentation, readinessTopicLabel } from '../../lib/format'

interface ReadinessMatrixProps {
  readiness: ReadinessEntry[]
}

/**
 * What the version knows, split into the four topics §7 names.
 *
 * The sentence above the list is not decoration and is not removable. Readiness
 * is the **availability of knowledge**. It is not permission to write to anybody,
 * not a promise that a product fits a case, and not an obligation the bureau has
 * taken on. Those three readings are what a green row invites, and they are the
 * readings that turn a knowledge base into a liability — so the caveat sits
 * immediately above the rows, in running text, not behind a tooltip.
 *
 * All four topics are always rendered. If the server sent no entry for one, the
 * row says so instead of vanishing: a matrix silently three rows long reads as
 * "the fourth is fine", which is the opposite of what a missing record means.
 *
 * `reason` is printed verbatim. It is the only place that says *why* a topic is
 * limited, and a rewritten reason is a reason somebody else made up.
 */
export function ReadinessMatrix({ readiness }: ReadinessMatrixProps) {
  const byTopic = new Map(readiness.map((entry) => [entry.topic, entry]))

  return (
    <div className="readiness">
      <p className="readiness__caveat">
        Готовность — это доступность знаний, а не разрешение на рассылку, не обещание
        совместимости и не принятое обязательство. «Знания есть» означает только то, что в
        опубликованной версии есть чем ответить.
      </p>

      <ul className="readiness__list" aria-label="Готовность знаний по четырём темам">
        {READINESS_TOPICS.map((topic) => {
          const entry = byTopic.get(topic) ?? null
          const presentation = entry ? readinessStatePresentation(entry.state) : null
          return (
            <li
              key={topic}
              className="readiness__item"
              data-topic={topic}
              data-state={entry?.state ?? 'unrecorded'}
            >
              <span className="readiness__topic">{readinessTopicLabel(topic)}</span>
              <span className="readiness__state" data-tone={presentation?.tone ?? 'neutral'}>
                {presentation ? presentation.label : 'готовность не записана'}
              </span>
              <span className="readiness__reason">
                {entry ? entry.reason : 'Сервер не прислал запись о готовности по этой теме.'}
              </span>
            </li>
          )
        })}
      </ul>
    </div>
  )
}

interface VersionGapListProps {
  gaps: VersionGap[]
  emptyNote?: string
}

/**
 * What the version does not know.
 *
 * It lives next to the matrix because a gap is the other half of readiness:
 * `blocks_topics` names exactly which of the four topics this gap holds back
 * (`block-01-spec.md` §11), so the two are read together or not at all. A gap
 * carries no quotation — there is nothing to quote — so it is rendered plainly:
 * what is missing, and what that costs.
 *
 * Gaps are never hidden. An answer that says "нет данных" without naming the gap
 * is indistinguishable from an answer that failed to look.
 */
export function VersionGapList({ gaps, emptyNote }: VersionGapListProps) {
  if (gaps.length === 0) {
    return <p className="knowledge-note">{emptyNote ?? 'Пробелов в этой версии не записано.'}</p>
  }

  return (
    <ul className="gap-list" aria-label="Пробелы знаний в версии">
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
          {gap.blocks_topics.length > 0 ? (
            <p className="gap__blocks">
              <span className="gap__label">ограничивает готовность:</span>{' '}
              {gap.blocks_topics.map(readinessTopicLabel).join(', ')}
            </p>
          ) : null}
        </li>
      ))}
    </ul>
  )
}
