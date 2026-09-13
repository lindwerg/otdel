import type { ResearchBudget } from '../../api/types'
import { formatMicros } from '../../lib/format'

interface BudgetMeterProps {
  budget: ResearchBudget
}

/**
 * The bureau's research money.
 *
 * Three things this deliberately does *not* do.
 *
 * It does not call the amounts an invoice: they are the owner's declared tariff
 * (`OTDEL_RESEARCH_COST_PER_SEARCH_MICROS`), applied to calls that were really
 * made. The footnote says so, because a number that looks like a bill and is not
 * one is worse than no number.
 *
 * It does not hide the "unknown" bucket. A request that left the machine and
 * never came back is counted as spent *and* shown separately, because somebody
 * has to reconcile it against the provider's own record.
 *
 * And the bar is a real proportion of real integers — not an estimate, not a
 * projection, and never rounded up to look tidy.
 */
export function BudgetMeter({ budget }: BudgetMeterProps) {
  const { currency, limit_micros: limit } = budget
  const spentShare = limit > 0 ? Math.min(1, budget.spent_micros / limit) : 0
  const reservedShare = limit > 0 ? Math.min(1 - spentShare, budget.reserved_micros / limit) : 0

  return (
    <section className="budget" aria-labelledby="budget-heading">
      <div className="budget__head">
        <h4 id="budget-heading">Бюджет на исследования</h4>
        <span className="budget__available">
          доступно {formatMicros(budget.available_micros, currency)} из{' '}
          {formatMicros(limit, currency)}
        </span>
      </div>

      <div
        className="budget__bar"
        role="img"
        aria-label={`Израсходовано ${formatMicros(budget.spent_micros, currency)}, зарезервировано ${formatMicros(budget.reserved_micros, currency)}, всего ${formatMicros(limit, currency)}`}
      >
        <span
          className="budget__bar-part budget__bar-part--spent"
          style={{ width: `${spentShare * 100}%` }}
        />
        <span
          className="budget__bar-part budget__bar-part--reserved"
          style={{ width: `${reservedShare * 100}%` }}
        />
      </div>

      <dl className="budget__figures">
        <div>
          <dt>израсходовано</dt>
          <dd>{formatMicros(budget.spent_micros, currency)}</dd>
        </div>
        <div>
          <dt>зарезервировано</dt>
          <dd>{formatMicros(budget.reserved_micros, currency)}</dd>
        </div>
        <div>
          <dt>на одно исследование</dt>
          <dd>{formatMicros(budget.plan_budget_micros, currency)}</dd>
        </div>
        <div>
          <dt>поиск / страница / вызов модели</dt>
          <dd>
            {formatMicros(budget.cost_per_search_micros, currency)} /{' '}
            {formatMicros(budget.cost_per_fetch_micros, currency)} /{' '}
            {formatMicros(budget.cost_per_model_call_micros, currency)}
          </dd>
        </div>
      </dl>

      {budget.unknown_micros > 0 ? (
        <p className="budget__unknown" role="status">
          Из них {formatMicros(budget.unknown_micros, currency)} — запросы с неизвестным исходом:
          они ушли к провайдеру, ответ не получен. Расход учтён, но его нужно сверить со счётом
          провайдера.
        </p>
      ) : null}

      <p className="budget__footnote">
        Суммы посчитаны по объявленному тарифу из настроек, а не по счёту провайдера. Перед
        каждым платным вызовом — поиском, загрузкой страницы и обращением к модели — деньги
        резервируются, после вызова расход фиксируется; параллельные задания не обходят общий
        лимит.
      </p>
    </section>
  )
}
