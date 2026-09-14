import { useState } from 'react'
import { askPublished } from '../../api/client'
import type { AnswerResponse, RetrievalLimits } from '../../api/types'
import { useAuth } from '../../auth/AuthContext'
import {
  answerStatePresentation,
  searchModeLabel,
  versionStatusPresentation,
} from '../../lib/format'
import { ClaimList, VersionEvidenceList } from './ClaimList'
import { ReadinessMatrix, VersionGapList } from './ReadinessMatrix'

interface AskPanelProps {
  partnerId: string
  limits: RetrievalLimits
}

/**
 * A question against one published version, and four honest answers to it.
 *
 * This is the screen where a knowledge base is most likely to lie, so each state
 * is rendered as itself rather than as a degraded version of `answered`.
 *
 * `answered` — the prose exists. It sits **under** a badge saying it is the
 * model's wording, because `answer_is_model_context` is true whenever `text` is
 * not null: prose is a formulation, never a quotation. Its citations are shown
 * with it, and the contract guarantees they are non-empty and belong to claims of
 * this pinned version — an answer whose citations did not resolve is downgraded
 * server-side to `evidence_only` with a reason in `rejections`.
 *
 * `evidence_only` — statements with citations exist and no prose was composed.
 * It is said plainly, in those words. Dressing this up as an answer would take
 * the one case where the system is being careful and make it look like a failure
 * to find anything.
 *
 * `insufficient_evidence` — there is a version and nothing in it fits. No prose,
 * no claims, no citations, and the gap named if one is recorded. Nothing is
 * guessed (`block-01-spec.md` §13.5).
 *
 * `no_published_version` — the partner has nothing published, because no check
 * ran, the check was blocked, or the version was retracted. It is a state, not
 * an error, and it is styled as one.
 *
 * `limitations[]` and `rejections[]` are always printed verbatim. Limited
 * readiness must never read as a full commercial clearance (§13.7), and the
 * reasons something was refused are the only way to tell a careful answer from
 * a lazy one.
 */
export function AskPanel({ partnerId, limits }: AskPanelProps) {
  const { runMutation } = useAuth()
  const [question, setQuestion] = useState('')
  const [busy, setBusy] = useState(false)
  const [answer, setAnswer] = useState<AnswerResponse | null>(null)
  const [error, setError] = useState<string | null>(null)

  async function submit(event: React.FormEvent) {
    event.preventDefault()
    const text = question.trim()
    if (text.length === 0) return
    setBusy(true)
    setError(null)
    try {
      // POST: the question text has no business in a URL, a log or browser history.
      const response = await runMutation((token) =>
        askPublished(partnerId, { question: text }, token),
      )
      setAnswer(response)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Не удалось получить ответ.')
    } finally {
      setBusy(false)
    }
  }

  const presentation = answer ? answerStatePresentation(answer.state) : null

  return (
    <section className="retrieval-box ask" aria-labelledby="ask-heading">
      <h3 id="ask-heading" className="knowledge-subheading">
        Вопрос по опубликованной версии
      </h3>
      <p className="knowledge-note">
        Отвечает только опубликованная версия. Если в ней нет подходящих утверждений, ответа не
        будет: догадка сюда не подставляется.
      </p>

      <form className="retrieval-form" onSubmit={(event) => void submit(event)}>
        <label className="field">
          <span>Вопрос</span>
          <input
            type="text"
            value={question}
            maxLength={limits.max_query_chars}
            onChange={(event) => setQuestion(event.target.value)}
            placeholder="например: какая минимальная толщина покрытия?"
          />
        </label>
        <button
          type="submit"
          className="button button-outline button-small"
          disabled={busy || question.trim().length === 0}
        >
          {busy ? 'Спрашиваем…' : 'Спросить'}
        </button>
      </form>

      {error ? (
        <p className="field-error" role="alert">
          {error}
        </p>
      ) : null}

      {answer && presentation ? (
        <div className="retrieval-result" data-state={answer.state}>
          <p className="retrieval-result__state" data-tone={presentation.tone}>
            {presentation.label}
          </p>
          <p className="retrieval-result__message">{answer.message || presentation.defaultHint}</p>

          <p className="retrieval-result__version">
            {answer.version
              ? `закреплённая версия ${answer.version.number} · ${versionStatusPresentation(answer.version.status).label}`
              : 'версия не закреплена: у партнёра нет опубликованной версии'}
          </p>

          <p className="retrieval-result__mode" data-mode={answer.mode}>
            режим поиска утверждений: {searchModeLabel(answer.mode)}
          </p>
          {answer.degraded.length > 0 ? (
            <ul className="retrieval-result__degraded" aria-label="Почему режим поиска неполный">
              {answer.degraded.map((reason) => (
                <li key={reason}>{reason}</li>
              ))}
            </ul>
          ) : null}

          {answer.state === 'answered' && answer.text ? (
            <div className="answer-text">
              <span className="evidence__badge evidence__badge--model">
                формулировка модели — не цитата источника
              </span>
              <p className="answer-text__body">{answer.text}</p>
            </div>
          ) : null}

          {answer.state === 'evidence_only' ? (
            <p className="answer-none">
              Связного ответа не составлено. Ниже — найденные утверждения версии с их цитатами;
              всё остальное пришлось бы придумать.
            </p>
          ) : null}

          {answer.state === 'insufficient_evidence' ? (
            <p className="answer-none">
              Ответа нет: в опубликованной версии не нашлось подходящих утверждений. Ниже названо,
              чего именно не хватает.
            </p>
          ) : null}

          {answer.state === 'no_published_version' ? (
            <p className="answer-none">
              У партнёра нет опубликованной версии знаний. Это состояние, а не ошибка: проверка
              либо не проходила, либо была заблокирована правилами готовности, либо версию
              отозвали. Отвечать пока не по чему.
            </p>
          ) : null}

          {answer.citations.length > 0 ? (
            <>
              <h4 className="retrieval-result__subheading">Цитаты, на которых стоит ответ</h4>
              <VersionEvidenceList partnerId={partnerId} evidence={answer.citations} />
            </>
          ) : null}

          {answer.claims.length > 0 ? (
            <>
              <h4 className="retrieval-result__subheading">Утверждения версии</h4>
              <ClaimList
                partnerId={partnerId}
                claims={answer.claims}
                label="Утверждения, найденные для ответа"
              />
            </>
          ) : null}

          {answer.conditions.length > 0 ? (
            <ul className="retrieval-result__conditions" aria-label="Условия из источников">
              {answer.conditions.map((item) => (
                <li key={item}>{item}</li>
              ))}
            </ul>
          ) : null}

          {answer.limitations.length > 0 ? (
            <div className="retrieval-result__limitations">
              <p className="retrieval-result__label">Оговорки — дословно, как ответил сервер:</p>
              <ul aria-label="Оговорки к ответу">
                {answer.limitations.map((item) => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
            </div>
          ) : null}

          {answer.rejections.length > 0 ? (
            <div className="retrieval-result__rejections">
              <p className="retrieval-result__label">Что было отклонено и почему:</p>
              <ul aria-label="Отклонённое при составлении ответа">
                {answer.rejections.map((item) => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
            </div>
          ) : null}

          {answer.gaps.length > 0 ? (
            <>
              <h4 className="retrieval-result__subheading">Чего версия не знает</h4>
              <VersionGapList gaps={answer.gaps} />
            </>
          ) : null}

          {answer.readiness.length > 0 ? <ReadinessMatrix readiness={answer.readiness} /> : null}
        </div>
      ) : null}
    </section>
  )
}
