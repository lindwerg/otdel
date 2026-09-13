import type { ProviderState } from '../../api/types'

interface ProviderNoticeProps {
  provider: ProviderState
}

/**
 * What the product role is waiting for.
 *
 * Shown whenever the model adapter is not ready, which is the current state of
 * this pilot: the OpenRouter key has not been supplied yet. The wording keeps
 * two things apart that are easy to confuse — reading materials works, and
 * structuring them into knowledge does not yet — and names the exact variables
 * to set. It never displays a key; the server does not send one.
 */
export function ProviderNotice({ provider }: ProviderNoticeProps) {
  if (provider.state === 'ready') {
    return (
      <p className="provider-line">
        Продуктолог: модель {provider.model}
        {provider.endpoint_host ? ` (${provider.endpoint_host})` : ''}.
      </p>
    )
  }

  return (
    <div className="provider-notice" role="status" data-state={provider.state}>
      <h4>
        {provider.state === 'disabled'
          ? 'Продуктолог отключён'
          : 'Продуктолог ожидает настройки'}
      </h4>
      <p>{provider.message}</p>
      {provider.missing.length > 0 ? (
        <ul className="provider-notice__missing">
          {provider.missing.map((name) => (
            <li key={name}>
              <code>{name}</code>
            </li>
          ))}
        </ul>
      ) : null}
      <p className="provider-notice__footnote">
        Чтение материалов и страницы источников работают как обычно. Знания не создаются
        и не выдумываются: пока провайдер не настроен, обращений к модели не происходит.
      </p>
    </div>
  )
}
