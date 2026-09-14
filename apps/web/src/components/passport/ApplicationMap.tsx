import { originalMaterialUrl } from '../../api/client'
import type { ProductApplication } from '../../api/types'
import { questionAudienceLabel } from '../../lib/format'
import { detailKindLabel } from '../../lib/passport'

interface ApplicationMapProps {
  partnerId: string
  applications: ProductApplication[]
  /** Name of the material, for the link back to the page. */
  materialFilename?: string
}

/**
 * Task → product → what to know → what limits it → what to ask.
 *
 * The order of the three detail kinds is fixed and is the point of the layout:
 * a buyer arrives with «чем закрепить лоток к бетону», not with «какая
 * безопасная рабочая нагрузка у BP21», and the answer is only usable if the
 * things that are *not* settled arrive with the things that are.
 *
 * A parameter and a constraint carry a quotation because they assert something.
 * A question carries an addressee and no quotation, because it asserts nothing —
 * and the markup says which is which rather than leaving the reader to infer it
 * from a missing block.
 */
export function ApplicationMap({ partnerId, applications }: ApplicationMapProps) {
  if (applications.length === 0) {
    return (
      <p className="knowledge-note">
        Задач применения не зафиксировано. Пустой список — не ответ: разбор должен либо
        назвать задачи, либо прямо сказать, что материал их не описывает.
      </p>
    )
  }

  return (
    <ul className="applications" aria-label="Карта применения">
      {applications.map((application) => (
        <li key={application.id} className="application">
          <header className="application__head">
            <h4 className="application__task">{application.task}</h4>
            <span className="application__product">
              {application.product_name ?? 'о предложении в целом'}
            </span>
          </header>

          {application.summary ? (
            <p className="application__summary">{application.summary}</p>
          ) : null}

          <blockquote className="application__quote">
            <span className="evidence__badge">цитата источника</span>
            <p>{application.quote}</p>
          </blockquote>
          <a
            className="evidence__source"
            href={originalMaterialUrl(partnerId, application.material_id, application.page_number)}
            target="_blank"
            rel="noreferrer"
          >
            страница {application.page_number}
          </a>

          {application.model_context ? (
            <p className="evidence__context">
              <span className="evidence__badge evidence__badge--model">
                пояснение модели — не цитата
              </span>
              {application.model_context}
            </p>
          ) : null}

          {application.details.length > 0 ? (
            <ul className="application__details">
              {application.details.map((detail) => (
                <li
                  key={detail.id}
                  className={`application__detail application__detail--${detail.kind}`}
                >
                  <span className="application__kind">{detailKindLabel(detail.kind)}</span>
                  <span className="application__label">{detail.label}</span>
                  {detail.value_text ? (
                    <span className="application__value">
                      {detail.unit ? `${detail.value_text} ${detail.unit}` : detail.value_text}
                    </span>
                  ) : null}
                  {detail.audience ? (
                    <span className="application__audience">
                      {questionAudienceLabel(detail.audience)} · подготовлен, канал не подключён
                    </span>
                  ) : null}
                  {detail.quote ? (
                    <blockquote className="application__detail-quote">{detail.quote}</blockquote>
                  ) : null}
                </li>
              ))}
            </ul>
          ) : null}
        </li>
      ))}
    </ul>
  )
}
