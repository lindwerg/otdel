import type { ResearchFinding } from '../../api/types'
import { formatDateTime } from '../../lib/format'

interface FindingListProps {
  findings: ResearchFinding[]
}

/**
 * Candidate conclusions about the **industry**.
 *
 * Everything about this list exists to stop one mistake: reading an industry
 * statement as a statement about the partner's product. So each entry carries a
 * scope badge, the whole list sits under a sentence that says it, and — because
 * labels are read second and layout first — the entries are visually unlike the
 * 1C product facts next door: no product name, no article, and the external
 * source is the loudest thing under the value.
 *
 * A citation here shows what a citation into the partner's own catalogue shows,
 * plus the two things an external source needs: the exact address, and the
 * moment it was read. The model's own sentence sits outside the quotation, under
 * its own badge, exactly as in 1C.
 */
export function FindingList({ findings }: FindingListProps) {
  if (findings.length === 0) {
    return (
      <p className="knowledge-note">
        Отраслевых выводов пока нет. Они появляются только из утверждённых вопросов, и только
        если найденный источник подтверждает вывод дословной цитатой.
      </p>
    )
  }

  return (
    <>
      <p className="knowledge-note">
        Это отраслевые сведения из внешних источников, а не характеристики продукции партнёра и
        не проверенные факты. Каждый вывод открывает страницу, на которой он написан.
      </p>
      <ul className="finding-list" aria-label="Отраслевые выводы">
        {findings.map((finding) => (
          <li key={finding.id} className="finding">
            <div className="finding__head">
              <span className="finding__scope">отраслевое сведение — не о партнёре</span>
              <span className="finding__topic">{finding.topic}</span>
            </div>

            <p className="finding__statement">
              <span className="finding__attribute">{finding.attribute}:</span>{' '}
              <span className="finding__value">
                {finding.unit ? `${finding.value_text} ${finding.unit}` : finding.value_text}
              </span>
            </p>

            {finding.conditions ? (
              <p className="finding__conditions">
                <span className="fact__label">условия:</span> {finding.conditions}
              </p>
            ) : null}

            <p className="finding__status">статус: кандидат, проверка — этап 1E</p>

            <div className="evidence">
              {finding.evidence.length === 0 ? (
                <p className="evidence__empty">
                  Источник не сохранён: утверждение не подтверждено.
                </p>
              ) : (
                <ul className="evidence__list">
                  {finding.evidence.map((item) => (
                    <li key={item.id} className="evidence__item">
                      <blockquote className="evidence__quote" cite={item.url}>
                        <span className="evidence__badge">цитата внешнего источника</span>
                        <p>{item.quote}</p>
                      </blockquote>
                      <a
                        className="evidence__source"
                        href={item.url}
                        target="_blank"
                        rel="noreferrer nofollow"
                      >
                        {item.host}
                      </a>
                      <p className="evidence__meta">
                        {item.retrieved_at
                          ? `прочитано ${formatDateTime(item.retrieved_at)}`
                          : 'дата чтения не сохранена'}
                        {item.license
                          ? ` · лицензия: ${item.license}`
                          : ' · лицензия не объявлена источником'}
                      </p>
                    </li>
                  ))}
                </ul>
              )}

              {finding.model_context ? (
                <p className="evidence__context">
                  <span className="evidence__badge evidence__badge--model">
                    пояснение модели — не цитата
                  </span>
                  {finding.model_context}
                </p>
              ) : null}
            </div>
          </li>
        ))}
      </ul>
    </>
  )
}
