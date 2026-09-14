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
  // A request may run the tool more than once, and every run is charged, so the
  // forecast says so out loud rather than letting the per-search price read as
  // the price of the request.
  const uses =
    engine.max_uses_per_request > 1
      ? ` До ${engine.max_uses_per_request} поисков в одном запросе — каждый оплачивается отдельно.`
      : ' Один поиск на запрос: больше провайдер не выполнит.'

  if (engine.search_base_micros === 0) {
    return `Поиск тарифицируется самим провайдером модели, отдельной цены за запрос нет; заложено ${tokens} на токены.${uses}`
  }
  if (engine.extra_result_micros === 0) {
    return `${base} за поиск независимо от числа результатов, плюс ${tokens} на токены модели.${uses}`
  }
  return (
    `${base} за поиск, включая ${engine.included_results} результатов; ` +
    `каждый следующий — ${formatMicros(engine.extra_result_micros, currency)}. ` +
    `Плюс ${tokens} на токены модели.${uses}`
  )
}

/**
 * Which engine will search, and what it is expected to cost.
 *
 * Shown before anything runs, because "сколько это будет стоить" is a question the
 * owner should be able to answer without starting a plan and reading an invoice
 * afterwards.
 *
 * Three things it refuses to be vague about.
 *
 * `auto` is resolved, not repeated. OpenRouter picks the model's own search when
 * the model has one and Exa when it does not, so for `openai/gpt-4o-mini` the
 * honest label is Exa and the honest price is Exa's — saying "auto" and leaving it
 * there would hide a real 0,007 USD per request behind a word. An engine chosen
 * explicitly, such as `perplexity`, is shown as itself and never as something it
 * might silently become.
 *
 * The number of *searches* is stated, not only the number of results. They are
 * different bounds and only one of them is what the bill counts.
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
          <dt>поисков на запрос</dt>
          <dd>{engine.max_uses_per_request}</dd>
        </div>
        <div>
          <dt>прогноз на один запрос</dt>
          <dd>{formatMicros(engine.forecast_micros, currency)}</dd>
        </div>
      </dl>

      {engine.search_domains.length > 0 ? (
        <p className="engine__domains">
          Поиск ограничен доменами: {engine.search_domains.join(', ')}. Это сужает выдачу
          движка; список разрешённых для чтения источников действует в любом случае.
        </p>
      ) : null}

      {engine.exa_fallback ? (
        <p className="engine__fallback" role="status">
          Модель {engine.model} не умеет искать сама, поэтому <code>auto</code> — это Exa, и
          считается тариф Exa. Чтобы платить за встроенный поиск провайдера, нужна модель,
          которая его поддерживает.
        </p>
      ) : null}

      <p className="engine__footnote">
        {tariffLine(engine, currency)} Это прогноз по объявленному тарифу — цене из
        документации провайдера, которая для этого движка ещё не сверялась с реальным
        счётом. В журнал записывается сумма, о которой отчитался провайдер, если он её
        сообщает.
        {engine.api_key_inherited
          ? ' Ключ взят из OTDEL_LLM_API_KEY — расходует тот же счёт, что и разбор материалов.'
          : ''}
      </p>
    </section>
  )
}
