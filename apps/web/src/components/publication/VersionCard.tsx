import type { KnowledgeVersion } from '../../api/types'
import { formatDateTime, versionCountsLine, versionStatusPresentation } from '../../lib/format'

interface VersionCardProps {
  version: KnowledgeVersion
}

/**
 * One immutable version, described by what it actually is.
 *
 * The card is built around a distinction that is easy to lose: a version
 * *exists* as the record of a check, and being published is a separate property
 * of it. So the status line never says «версия готова» — it says which of the six
 * states this record is in — and a `blocked` version prints its `blocked_reasons`
 * verbatim right under the status. Those reasons are the only honest answer to
 * "why is my knowledge not searchable", and summarising them ("не прошла
 * проверку") would throw away the part that tells the owner what to fix.
 *
 * `revoked_reason` is shown for the same reason it is mandatory on the server: a
 * withdrawal without a stated reason is indistinguishable from a malfunction.
 *
 * The counters are counts, never a share. «подтверждено источником: 12 из 40» is
 * a fact; «70% готово» would be a grade this system has no way to award, and it
 * would quietly hide the 28 statements that are not confirmed by any source.
 */
export function VersionCard({ version }: VersionCardProps) {
  const presentation = versionStatusPresentation(version.status)
  const isPublished = version.status === 'published'

  return (
    <article className="version" data-state={version.status}>
      <div className="version__head">
        <strong className="version__number">Версия {version.number}</strong>
        <span className="version__status" data-tone={presentation.tone}>
          {presentation.label}
        </span>
      </div>

      <p className="version__hint">{presentation.defaultHint}</p>
      <p className="version__counts">{versionCountsLine(version)}</p>

      <p className="version__meta">
        {isPublished && version.published_at
          ? `опубликована ${formatDateTime(version.published_at)}`
          : version.published_at
            ? `публиковалась ${formatDateTime(version.published_at)}`
            : 'не публиковалась'}
        {' · '}
        собрана {formatDateTime(version.created_at)}
        {version.superseded_at ? ` · заменена ${formatDateTime(version.superseded_at)}` : ''}
        {version.revoked_at ? ` · отозвана ${formatDateTime(version.revoked_at)}` : ''}
      </p>

      <p className="version__chunks">
        фрагментов для поиска: {version.chunks_total} · с векторами: {version.chunks_embedded}
        {version.embedding_profile ? ` · профиль ${version.embedding_profile}` : ''}
        {version.chunks_embedded === 0
          ? ' — векторов нет, поиск по этой версии идёт по ключевым словам'
          : ''}
      </p>

      {version.blocked_reasons.length > 0 ? (
        <div className="version__blocked">
          <p className="version__blocked-title">
            Почему версия не опубликована — дословно, как ответил сервер:
          </p>
          <ul aria-label="Причины, по которым версия не опубликована">
            {version.blocked_reasons.map((reason) => (
              <li key={reason}>{reason}</li>
            ))}
          </ul>
        </div>
      ) : null}

      {version.revoked_reason ? (
        <p className="version__revoked">
          <span className="version__label">причина отзыва:</span> {version.revoked_reason}
        </p>
      ) : null}

      <p className="version__fingerprint">отпечаток входа: {version.input_fingerprint}</p>
    </article>
  )
}
