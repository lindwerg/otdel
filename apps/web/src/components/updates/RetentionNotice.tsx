import type { RetentionPolicy } from '../../api/types'
import { formatDateTime } from '../../lib/format'

interface RetentionNoticeProps {
  policy: RetentionPolicy
}

/**
 * How long operational history is kept on this installation — and, more
 * importantly, what is never removed.
 *
 * The protected list is the policy's larger half and is shown in full. A
 * retention notice that only said "храним 90 дней" would leave the owner
 * reasonably afraid that a published version they cited could disappear; it
 * cannot, and the sentence saying so comes from the server.
 */
export function RetentionNotice({ policy }: RetentionNoticeProps) {
  return (
    <section className="retention" data-state={policy.state} aria-labelledby="retention-heading">
      <h3 id="retention-heading" className="section-heading">
        Хранение истории
      </h3>
      <p className="retention__message">{policy.message}</p>

      <p className="retention__counts">
        {[
          `событий в журнале: ${policy.preview.events_total}`,
          `из них под очистку: ${policy.preview.events_prunable}`,
          `заданий: ${policy.preview.jobs_total}`,
          `из них под очистку: ${policy.preview.jobs_prunable}`,
        ].join(' · ')}
      </p>
      {policy.preview.oldest_event ? (
        <p className="retention__oldest">
          Самая ранняя запись: {formatDateTime(policy.preview.oldest_event)}
        </p>
      ) : null}

      <div className="retention__protected">
        <p className="run__label">Очистка никогда не удаляет:</p>
        <ul aria-label="Что защищено от очистки">
          {policy.protected.map((line) => (
            <li key={line}>{line}</li>
          ))}
        </ul>
      </div>

      {/* A sweep records itself only when it removed something, so the absence of a
          record does not mean the sweep never ran — with a policy configured it runs on
          a schedule and usually finds nothing to do. Saying «ещё ни разу не выполнялась»
          about that would be a state nobody computed. */}
      {policy.last_sweep ? (
        <p className="retention__last">
          Последняя очистка, что-то удалившая: {formatDateTime(policy.last_sweep.occurred_at)} —{' '}
          {policy.last_sweep.summary}
        </p>
      ) : policy.state === 'enabled' ? (
        <p className="retention__last">
          Очистка включена и выполняется по расписанию; пока ни один проход ничего не
          удалял — записей об удалении в журнале нет.
        </p>
      ) : (
        <p className="retention__last">Очистка не выполняется: она выключена.</p>
      )}
    </section>
  )
}
