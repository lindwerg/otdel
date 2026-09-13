import type { RetrievalProviderState } from '../../api/types'
import { searchModeLabel } from '../../lib/format'

interface RetrievalNoticeProps {
  provider: RetrievalProviderState
}

/**
 * What this installation can and cannot do with published knowledge.
 *
 * The whole point of this block is to refuse one very natural — and wrong —
 * inference: that without a model key nothing here works. Checking and
 * publishing are deterministic rules and need no model at all
 * (`block-01-spec.md` §6.7: two models agreeing is not evidence). Exactly two
 * optional halves depend on an adapter — the prose answer, and the vector side
 * of search — and so the notice names those two and nothing else. A banner that
 * simply said «модель не настроена» would make an owner believe verification was
 * blocked, and the most likely reaction to that belief is to stop checking.
 *
 * When something is missing, the environment variables are named as `<code>`,
 * because "настройте провайдера" is not an instruction anybody can follow. No key
 * is ever displayed: the server does not send one.
 *
 * When everything is ready the notice stays. It then states which mode search is
 * actually in — `search_mode` is the fact, not the intention — so nobody has to
 * infer from result quality whether vectors are live.
 */
export function RetrievalNotice({ provider }: RetrievalNoticeProps) {
  const { vector, validation, embedding, answer } = provider

  return (
    <div className="retrieval-notice" data-state={provider.state} role="status">
      <h4>
        {provider.state === 'ready'
          ? 'Поиск и ответы настроены полностью'
          : 'Проверка и публикация работают; надстройки над ними — нет'}
      </h4>

      <p className="retrieval-notice__line">{provider.message}</p>

      <p className="retrieval-notice__validation">
        Проверка и публикация: {validation.message} Модель для этого не нужна вовсе — правила
        детерминированы. Модель и embeddings нужны только двум надстройкам: прозаическому ответу
        и векторной половине поиска.
      </p>

      {provider.state !== 'ready' && provider.missing.length > 0 ? (
        <ul className="retrieval-notice__missing" aria-label="Незаданные переменные окружения">
          {provider.missing.map((name) => (
            <li key={name}>
              <code>{name}</code>
            </li>
          ))}
        </ul>
      ) : null}

      <dl className="retrieval-notice__adapters">
        <dt>ответы</dt>
        <dd data-state={answer.state}>{answer.message}</dd>
        <dt>embeddings</dt>
        <dd data-state={embedding.state}>{embedding.message}</dd>
        <dt>векторы</dt>
        <dd data-state={vector.state}>
          {vector.message}
          {vector.profile ? ` · профиль ${vector.profile}` : ''}
        </dd>
      </dl>

      <p className="retrieval-notice__footnote">
        Фактический режим поиска:{' '}
        {searchModeLabel(provider.search_mode === 'hybrid' ? 'hybrid' : 'keyword')}. Пока
        embedding-провайдера нет, поиск работает по ключевым словам и честно об этом говорит, а
        ответ показывается найденными утверждениями с цитатами, без связного текста.
      </p>
    </div>
  )
}
