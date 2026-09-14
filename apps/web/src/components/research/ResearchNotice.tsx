import type { ResearchProviderState } from '../../api/types'

interface ResearchNoticeProps {
  provider: ResearchProviderState
}

/**
 * What the researcher is waiting for, and what it is allowed to do once it runs.
 *
 * Shown whenever the adapters are not all ready, which is the current state of
 * this pilot: no search provider has been chosen for OTDEL. The wording keeps
 * apart two things that are easy to confuse — reading and understanding the
 * partner's own materials works; asking the outside world does not yet — and
 * names the exact variables to set.
 *
 * When it *is* ready, the notice does not disappear. It becomes the statement of
 * what the researcher may do: which service is asked, which hosts may be read,
 * and what the per-plan bounds are. A component that reaches the internet on the
 * owner's money should say so while it is switched on, not only while it is off.
 */
export function ResearchNotice({ provider }: ResearchNoticeProps) {
  const { limits } = provider

  if (provider.state === 'ready') {
    return (
      <div className="research-notice" data-state="ready">
        <p className="research-notice__line">{provider.message}</p>
        <p className="research-notice__footnote">
          На одно исследование: не больше {limits.max_queries_per_plan} поисковых запросов и{' '}
          {limits.max_sources_per_plan} прочитанных страниц, не дольше{' '}
          {limits.plan_time_budget_seconds} с, не больше {limits.max_passes_per_plan} проходов.
          Читаются только объявленные хосты; переадресации не выполняются, robots.txt
          соблюдается.
        </p>
      </div>
    )
  }

  return (
    <div className="research-notice" role="status" data-state={provider.state}>
      <h4>
        {provider.state === 'disabled'
          ? 'Исследователь отключён'
          : 'Исследователь ожидает настройки'}
      </h4>
      <p>{provider.message}</p>
      {provider.missing.length > 0 ? (
        <ul className="research-notice__missing">
          {provider.missing.map((name) => (
            <li key={name}>
              <code>{name}</code>
            </li>
          ))}
        </ul>
      ) : null}
      <p className="research-notice__footnote">
        Материалы партнёра читаются и разбираются как обычно. Пока это не настроено, ни один
        внешний запрос не выполняется и бюджет не расходуется.
      </p>
    </div>
  )
}
