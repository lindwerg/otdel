import type { ResearchEngineState } from '../../api/types'
import { formatMicros } from '../../lib/format'

interface EngineNoticeProps {
  engine: ResearchEngineState
  currency: string
}

/** How the tariff is built, in the owner's words rather than in variable names. */
function tariffLine(engine: ResearchEngineState, currency: string): string {
  const base = formatMicros(engine.search_base_micros, currency)
  const tokens = formatMicros(engine.token_allowance_micros, currency)

  if (engine.search_base_micros === 0) {
    return `Поиск тарифицируется самим провайдером модели, отдельной цены за запрос нет; заложено ${tokens} на токены.`
  }
  if (engine.extra_result_micros === 0) {
    return `${base} за запрос независимо от числа результатов, плюс ${tokens} на токены модели.`
  }
  return (
    `${base} за запрос, включая ${engine.included_results} результатов; ` +
    `каждый следующий — ${formatMicros(engine.extra_result_micros, currency)}. ` +
    `Плюс ${tokens} на токены модели.`
  )
}

/**
 * Which engine will search, and what it is expected to cost.
 *
 * Shown before anything runs, because "сколько это будет стоить" is a question the
 * owner should be able to answer without starting a plan and reading an invoice
 * afterwards.
 *
 * Two things it refuses to be vague about.
 *
 * `auto` is resolved, not repeated. OpenRouter picks the model's own search when
 * the model has one and Exa when it does not, so for `openai/gpt-4o-mini` the
 * honest label is Exa and the honest price is Exa's — saying "auto" and leaving it
 * there would hide a real 0,007 USD per request behind a word.
 *
 * And the forecast is labelled a forecast. The ledger records what the provider
 * reported charging, which is a different number, and the two are never shown as
 * if they were one.
 */
export function EngineNotice({ engine, currency }: EngineNoticeProps) {
  return (
    <section className="engine" aria-labelledby="engine-heading">
      <div className="engine__head">
        <h4 id="engine-heading">Поиск</h4>
        <span className="engine__badge" data-engine={engine.effective}>
          {engine.effective}
        </span>
      </div>

      <dl className="engine__figures">
        <div>
          <dt>движок</dt>
          <dd>
            {engine.configured === engine.effective
              ? engine.effective
              : `${engine.configured} → ${engine.effective}`}
          </dd>
        </div>
        <div>
          <dt>модель</dt>
          <dd>{engine.model}</dd>
        </div>
        <div>
          <dt>результатов на запрос</dt>
          <dd>
            {engine.max_results} (не больше {engine.max_total_results_per_plan} на всё
            исследование)
          </dd>
        </div>
        <div>
          <dt>прогноз на один поиск</dt>
          <dd>{formatMicros(engine.forecast_micros, currency)}</dd>
        </div>
      </dl>

      {engine.exa_fallback ? (
        <p className="engine__fallback" role="status">
          Модель {engine.model} не умеет искать сама, поэтому <code>auto</code> — это Exa, и
          считается тариф Exa. Чтобы платить за встроенный поиск провайдера, нужна модель,
          которая его поддерживает.
        </p>
      ) : null}

      <p className="engine__footnote">
        {tariffLine(engine, currency)} Это прогноз по объявленному тарифу: в журнал
        записывается сумма, о которой отчитался провайдер, если он её сообщает.
        {engine.api_key_inherited
          ? ' Ключ взят из OTDEL_LLM_API_KEY — расходует тот же счёт, что и разбор материалов.'
          : ''}
      </p>
    </section>
  )
}
