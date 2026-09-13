import type { HistoryEvent } from '../../api/types'
import { eventActorLabel, eventKindLabel, formatDateTime } from '../../lib/format'

interface EventLogProps {
  events: HistoryEvent[]
}

/**
 * The partner's history, newest first.
 *
 * Every line is the server's own sentence. The component adds a title for the
 * kind and says who caused it — and nothing else: a history assembled in the
 * browser from an enum would describe what the interface believes happened
 * rather than what did.
 *
 * The log is append-only in the database, so nothing here offers to edit or
 * remove a line. Entries leave only through the retention sweep, which records
 * that it ran.
 */
export function EventLog({ events }: EventLogProps) {
  return (
    <section className="history" aria-labelledby="history-heading">
      <h3 id="history-heading" className="section-heading">
        Что происходило
      </h3>
      <p className="knowledge-note">
        Журнал дописывается и не редактируется. Строки удаляет только настроенная очистка
        истории, и она записывает сама себя.
      </p>

      {events.length === 0 ? (
        <p className="empty-state">Событий пока нет.</p>
      ) : (
        <ol className="history__list" aria-label="Журнал событий партнёра">
          {events.map((event) => (
            <li key={event.id} className="history__item" data-kind={event.kind}>
              <div className="history__head">
                <span className="history__kind">{eventKindLabel(event.kind)}</span>
                <span className="history__actor">{eventActorLabel(event.actor)}</span>
                <time className="history__time" dateTime={event.occurred_at}>
                  {formatDateTime(event.occurred_at)}
                </time>
              </div>
              <p className="history__summary">{event.summary}</p>
            </li>
          ))}
        </ol>
      )}
    </section>
  )
}
