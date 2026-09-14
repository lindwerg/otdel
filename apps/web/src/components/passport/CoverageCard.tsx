import type { CoverageReport } from '../../api/types'
import {
  coverageLine,
  passLine,
  purposeLabel,
  coverageStateLabel,
  coverageTone,
  costLine,
  dispositionLabel,
  declarationTopicLabel,
  requirementsLabel,
  requirementsTone,
  splitRequirement,
} from '../../lib/passport'

interface CoverageCardProps {
  report: CoverageReport
}

/**
 * What one run covered, what it judged, and what it left.
 *
 * The card is laid out around one claim it refuses to let the reader miss: the
 * denominator. «36 страниц» and «36 из 44» are different sentences, and only
 * the second one can be acted on — the audited run reported the first and was
 * recorded as a success.
 *
 * The two verdicts sit side by side and are never merged into one badge. A run
 * can have read every page and produced nothing worth publishing, or produced
 * plenty from half a document; a single «готово / не готово» would flatten both
 * into the same word and lose which of the two has to be fixed.
 */
export function CoverageCard({ report }: CoverageCardProps) {
  const cost = costLine(report.cost_micro_usd)

  return (
    <article className="coverage" aria-label={`Охват материала «${report.material_filename}»`}>
      <header className="coverage__head">
        <h4 className="coverage__title">{report.material_filename}</h4>
        <p className="coverage__count">{coverageLine(report)}</p>
      </header>

      <div className="coverage__verdicts">
        <span className={`coverage__badge coverage__badge--${coverageTone(report.state)}`}>
          {coverageStateLabel(report.state)}
        </span>
        <span
          className={`coverage__badge coverage__badge--${requirementsTone(report.requirements)}`}
        >
          {requirementsLabel(report.requirements)}
        </span>
      </div>

      {report.allows_automatic_publication ? (
        <p className="coverage__gate coverage__gate--open">
          Материал можно публиковать без ручной проверки.
        </p>
      ) : (
        <p className="coverage__gate coverage__gate--shut">
          Публиковать автоматически нельзя: нужен человек.
        </p>
      )}

      {report.requirements_missing.length > 0 ? (
        <section className="coverage__missing">
          <h5>Чего не хватает паспорту</h5>
          <ul>
            {report.requirements_missing.map((line) => {
              const item = splitRequirement(line)
              return (
                <li key={line}>
                  {item.name ? <code className="coverage__name">{item.name}</code> : null}
                  <span>{item.explanation}</span>
                </li>
              )
            })}
          </ul>
        </section>
      ) : null}

      {report.declarations.length > 0 ? (
        <section className="coverage__declared">
          <h5>Разбор прямо заявил, что в материале этого нет</h5>
          <ul>
            {report.declarations.map((declaration) => (
              <li key={declaration.id}>
                <span className="coverage__topic">
                  {declarationTopicLabel(declaration.topic)}
                </span>
                <span className="coverage__stated">{declaration.stated}</span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {/* R05.2. «44 из 44» is true and cannot explain an empty glossary; these can.
          Unfinished passes come first, because they are the ones that mean an empty
          section is an open question rather than a finding about the material. */}
      {report.passes.length > 0 ? (
        <section className="coverage__passes">
          <h5>Что искали по отдельности</h5>
          <ul>
            {[...report.passes]
              .sort((a, b) => Number(a.covered_everything) - Number(b.covered_everything))
              .map((pass) => (
                <li
                  key={pass.id}
                  className={
                    pass.covered_everything
                      ? 'coverage__pass'
                      : 'coverage__pass coverage__pass--unfinished'
                  }
                >
                  <span className="coverage__pass-name">{purposeLabel(pass.purpose)}</span>
                  <span className="coverage__pass-count">{passLine(pass)}</span>
                  <span className="coverage__pass-requests">
                    запросов {pass.requests_made} из {pass.requests_allowed}
                  </span>
                </li>
              ))}
          </ul>
        </section>
      ) : null}

      {report.notes.length > 0 ? (
        <section className="coverage__notes">
          <h5>Почему разобрано не всё</h5>
          <ul>
            {report.notes.map((note) => (
              <li key={note}>{note}</li>
            ))}
          </ul>
        </section>
      ) : null}

      {/* Every page, not only the unhappy ones: six explained pages beside an
          unstated total is the report the audit could not act on. */}
      <details className="coverage__pages">
        <summary>Постранично: {report.pages.length}</summary>
        <ul>
          {report.pages.map((page) => (
            <li key={page.id} className={`coverage__page coverage__page--${page.disposition}`}>
              <span className="coverage__page-number">с. {page.page_number}</span>
              <span className="coverage__page-state">{dispositionLabel(page.disposition)}</span>
              {page.reason && page.reason !== dispositionLabel(page.disposition) ? (
                <span className="coverage__page-reason">{page.reason}</span>
              ) : null}
            </li>
          ))}
        </ul>
      </details>

      <footer className="coverage__foot">
        {report.resumable_pages.length > 0 ? (
          <span className="coverage__resumable">
            Ждут следующего прохода: с.&nbsp;{report.resumable_pages.join(', ')}
          </span>
        ) : null}
        <span className="coverage__cost">
          {/* A missing cost is "the provider did not say", never "free". */}
          {cost ? `стоимость прохода: ${cost}` : 'стоимость прохода: провайдер не сообщил'}
        </span>
      </footer>
    </article>
  )
}
