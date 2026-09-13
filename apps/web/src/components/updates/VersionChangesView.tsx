import type { VersionChanges, VersionStatus } from '../../api/types'
import {
  changeCountsLine,
  changeKindLabel,
  claimStatusPresentation,
  readinessStatePresentation,
  readinessTopicLabel,
  versionStatusPresentation,
} from '../../lib/format'

interface VersionChangesViewProps {
  changes: VersionChanges
}

/**
 * What one version says that the previous one did not.
 *
 * The two things this view must not do, and does not:
 *
 * * **turn a disappearance into a refutation.** A claim that is no longer in the
 *   version is shown with the server's sentence, which says explicitly that the
 *   source may have changed, been re-read or become unreadable;
 * * **hide what the comparison cannot see.** `limitations[]` is rendered beside
 *   the result, not in a footnote — a renamed property reads here as one removal
 *   and one addition, and the reader has to know that.
 */
export function VersionChangesView({ changes }: VersionChangesViewProps) {
  const presentation = versionStatusPresentation(changes.to.status as VersionStatus)

  return (
    <section className="changes" aria-labelledby="changes-heading">
      <h3 id="changes-heading" className="section-heading">
        Что изменилось в версии {changes.to.number}
      </h3>
      {/* The status of the compared version, always. `to` may be a version that was never
          published — the panel offers the newest one when nothing is published — and a
          diff of a rejected snapshot rendered without saying so reads as the live one. */}
      <p className="changes__status" data-tone={presentation.tone}>
        {presentation.label}
      </p>
      {changes.to.status !== 'published' ? (
        <p className="changes__not-live">
          По этой версии поиск и ответы не выполняются.
        </p>
      ) : null}
      <p className="changes__summary">{changes.message}</p>
      <p className="changes__counts">{changeCountsLine(changes.counts)}</p>
      {changes.from ? (
        <p className="knowledge-note">
          Сравнение с версией {changes.from.number}. Обе версии неизменны: сравниваются
          снимки, а не документы.
        </p>
      ) : (
        <p className="knowledge-note">
          Это первая версия партнёра — сравнивать не с чем, поэтому всё в ней показано как
          добавленное.
        </p>
      )}

      {changes.readiness.length > 0 ? (
        <div className="changes__readiness">
          <p className="run__label">Готовность</p>
          <ul aria-label="Изменения готовности">
            {changes.readiness.map((entry) => (
              <li key={entry.topic}>
                <span className="changes__topic">{readinessTopicLabel(entry.topic)}</span>
                <span className="changes__readiness-move">
                  {entry.before ? readinessStatePresentation(entry.before).label : '—'} →{' '}
                  {entry.after ? readinessStatePresentation(entry.after).label : '—'}
                </span>
                <span className="changes__reason">{entry.reason}</span>
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {changes.claims.length === 0 ? (
        <p className="empty-state">Утверждения не изменились.</p>
      ) : (
        <ul className="changes__claims" aria-label="Изменения утверждений">
          {changes.claims.map((change, index) => (
            <li
              key={`${change.kind}-${change.attribute}-${index}`}
              className="changes__claim"
              data-kind={change.kind}
            >
              <div className="changes__claim-head">
                <span className="changes__kind">{changeKindLabel(change.kind)}</span>
                {change.product_name ? (
                  <span className="changes__product">{change.product_name}</span>
                ) : (
                  <span className="changes__product changes__product--scope">
                    {change.scope === 'industry' ? 'отраслевое' : 'о предложении в целом'}
                  </span>
                )}
                <span className="changes__attribute">{change.attribute}</span>
              </div>
              <p className="changes__message">{change.message}</p>
              <div className="changes__sides">
                {change.before ? (
                  <div className="changes__side" data-side="before">
                    <span className="changes__side-label">было</span>
                    <span className="changes__value">
                      {change.before.value_text}
                      {change.before.unit ? ` ${change.before.unit}` : ''}
                    </span>
                    <span
                      className="claim__verdict"
                      data-tone={claimStatusPresentation(change.before.status).tone}
                    >
                      {claimStatusPresentation(change.before.status).label}
                    </span>
                    <span className="changes__sources">
                      {change.before.sources.join(', ') || 'источник не записан'}
                    </span>
                  </div>
                ) : null}
                {change.after ? (
                  <div className="changes__side" data-side="after">
                    <span className="changes__side-label">стало</span>
                    <span className="changes__value">
                      {change.after.value_text}
                      {change.after.unit ? ` ${change.after.unit}` : ''}
                    </span>
                    <span
                      className="claim__verdict"
                      data-tone={claimStatusPresentation(change.after.status).tone}
                    >
                      {claimStatusPresentation(change.after.status).label}
                    </span>
                    <span className="changes__sources">
                      {change.after.sources.join(', ') || 'источник не записан'}
                    </span>
                  </div>
                ) : null}
              </div>
            </li>
          ))}
        </ul>
      )}

      {changes.gaps.length > 0 ? (
        <div className="changes__gaps">
          <p className="run__label">Пробелы</p>
          <ul aria-label="Изменения пробелов">
            {changes.gaps.map((gap, index) => (
              <li key={`${gap.kind}-${gap.topic}-${index}`} data-kind={gap.kind}>
                <span className="changes__kind">{changeKindLabel(gap.kind)}</span>
                <span className="changes__topic">{gap.topic}</span>
                <span className="changes__reason">{gap.missing}</span>
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      <div className="changes__limitations">
        <p className="run__label">Чего это сравнение не видит:</p>
        <ul aria-label="Ограничения сравнения">
          {changes.limitations.map((line) => (
            <li key={line}>{line}</li>
          ))}
        </ul>
      </div>
    </section>
  )
}
