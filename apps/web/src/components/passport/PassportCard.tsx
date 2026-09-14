import type { ProductPassport } from '../../api/types'
import { factValueLine, questionAudienceLabel } from '../../lib/format'
import {
  aliasRelationLabel,
  gapNatureLabel,
  identityBasisLabel,
  identityStateLabel,
  isSafeToFollow,
  structuralSourceLabel,
} from '../../lib/passport'
import { EvidenceList } from '../knowledge/EvidenceList'
import { ApplicationMap } from './ApplicationMap'
import { UncertaintyList } from './UncertaintyList'

interface PassportCardProps {
  partnerId: string
  passport: ProductPassport
}

/**
 * Everything known about one product — including what is not known.
 *
 * The card has a fixed order and the order is the argument: what the product is,
 * what it is for, what is stated about it, what is missing, what cannot be read,
 * and what might be the same thing in another catalogue. A reader who stops
 * scrolling after the facts has seen the part that was always easy to produce;
 * the sections below are the ones the audited pipeline produced none of.
 *
 * Three renderings are deliberate rather than decorative:
 *
 * * an alias whose relation is `unclear` is shown with a warning marker and is
 *   never presented as another name for the product — following it is exactly
 *   the merge nobody proved;
 * * a fact read out of a table says which column and which row it came from,
 *   beside its quotation and never instead of it;
 * * an identity proposal names what it rests on, and a proposal resting on
 *   nothing but similar names says so in those words.
 */
export function PassportCard({ partnerId, passport }: PassportCardProps) {
  const product = passport.product
  const substantive =
    product.summary != null ||
    passport.facts.length > 0 ||
    passport.applications.length > 0 ||
    passport.gaps.length > 0

  return (
    <article className="passport" aria-label={`Паспорт изделия «${product.name}»`}>
      <header className="passport__head">
        <h3 className="passport__name">{product.name}</h3>
        <span className="passport__source">
          {passport.category ? `${passport.category.name} · ` : ''}
          {passport.material_filename}
        </span>
      </header>

      {product.summary ? <p className="passport__summary">{product.summary}</p> : null}

      {!substantive ? (
        // The audited run produced forty-four of exactly this. Saying so beats
        // rendering a name and letting the empty space read as completeness.
        <p className="passport__thin">
          Кроме названия, по этому изделию в материале ничего не нашлось. Это не паспорт —
          это строка каталога.
        </p>
      ) : null}

      {passport.aliases.length > 0 ? (
        <section className="passport__section">
          <h4>Другие написания в этом материале</h4>
          <ul className="aliases">
            {passport.aliases.map((alias) => (
              <li
                key={alias.id}
                className={`alias ${isSafeToFollow(alias.relation) ? '' : 'alias--unclear'}`}
              >
                <span className="alias__surface">{alias.surface}</span>
                <span className="alias__relation">{aliasRelationLabel(alias.relation)}</span>
                {alias.note ? <span className="alias__note">{alias.note}</span> : null}
                <blockquote className="alias__quote">{alias.quote}</blockquote>
                <span className="alias__page">с. {alias.page_number}</span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      <section className="passport__section">
        <h4>Для чего это нужно</h4>
        <ApplicationMap partnerId={partnerId} applications={passport.applications} />
      </section>

      <section className="passport__section">
        <h4>Что сказано в материале</h4>
        {passport.facts.length === 0 ? (
          <p className="knowledge-note">Характеристик не зафиксировано.</p>
        ) : (
          <ul className="fact-list">
            {passport.facts.map((fact) => (
              <li key={fact.id} className="fact">
                <div className="fact__head">
                  <span className="fact__attribute">{fact.attribute}</span>
                  <span className="fact__value">{factValueLine(fact)}</span>
                </div>
                {fact.conditions ? (
                  <p className="fact__conditions">при условии: {fact.conditions}</p>
                ) : null}

                {/* Provenance of a second kind: which structure the number sat
                    in. Shown beside the quotation, never instead of it. */}
                <p className="fact__origin">
                  <span className="fact__origin-label">
                    {structuralSourceLabel(fact.origin.source)}
                  </span>
                  {fact.origin.source === 'table_cell' && fact.origin.subject ? (
                    <span className="fact__origin-context">
                      {fact.origin.subject} · {fact.origin.property}
                      {fact.origin.conditions.length > 0
                        ? ` · ${fact.origin.conditions.join('; ')}`
                        : ''}
                    </span>
                  ) : null}
                </p>

                <EvidenceList
                  partnerId={partnerId}
                  evidence={fact.evidence}
                  modelContext={fact.model_context}
                />
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="passport__section">
        <h4>Чего в материале нет</h4>
        {passport.gaps.length === 0 ? (
          <p className="knowledge-note">Пробелов не зафиксировано.</p>
        ) : (
          <ul className="gap-list">
            {passport.gaps.map((gap) => (
              <li key={gap.id} className={`gap gap--${gap.nature}`}>
                <div className="gap__head">
                  <span className="gap__topic">{gap.topic}</span>
                  <span className="gap__nature">{gapNatureLabel(gap.nature)}</span>
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
                      {questionAudienceLabel(gap.question.audience)} · подготовлен, канал не
                      подключён
                    </span>
                    <p>{gap.question.text}</p>
                  </div>
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="passport__section">
        <h4>Что нельзя прочитать однозначно</h4>
        <UncertaintyList uncertainties={passport.uncertainties} />
      </section>

      {passport.identity_links.length > 0 ? (
        <section className="passport__section">
          <h4>Возможно, то же изделие в другом материале</h4>
          <ul className="identity">
            {passport.identity_links.map((link) => (
              <li key={link.id} className={`identity__item identity__item--${link.state}`}>
                <span className="identity__state">{identityStateLabel(link.state)}</span>
                <span className="identity__basis">{identityBasisLabel(link.basis)}</span>
                {link.note ? <p className="identity__note">{link.note}</p> : null}
                {/* Stated plainly: the rows are not merged, and nothing here
                    answers a question as if they were one product. */}
                <p className="identity__caution">
                  Записи остаются раздельными: система ничего не объединяет сама.
                </p>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
    </article>
  )
}
