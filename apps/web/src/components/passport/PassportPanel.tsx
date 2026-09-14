import { usePassports } from '../../hooks/usePassports'
import { StatusMessage } from '../StatusMessage'
import { ApplicationMap } from './ApplicationMap'
import { CoverageCard } from './CoverageCard'
import { PassportCard } from './PassportCard'
import { UncertaintyList } from './UncertaintyList'

interface PassportPanelProps {
  partnerId: string
}

/**
 * The product base: what is known, out of how much, and what is still open.
 *
 * The panel opens with the page account rather than with the products, and that
 * ordering is the whole argument of this surface. A list of forty-four products
 * is a satisfying thing to look at; «разобрано 36 из 44, паспорту не хватает
 * данных» above it is the sentence that tells the reader whether the list means
 * anything. Putting the account second would recreate the report the audit could
 * not act on, in a nicer font.
 */
export function PassportPanel({ partnerId }: PassportPanelProps) {
  const { data, error, reload } = usePassports(partnerId)

  if (error) {
    return (
      <StatusMessage tone="error" onRetry={reload}>
        {error}
      </StatusMessage>
    )
  }
  if (!data) {
    return <p className="knowledge-note">Загружаем продуктовую базу…</p>
  }

  const { passports, coverage, applications, uncertainties } = data
  const blocked = coverage.filter((report) => !report.allows_automatic_publication)
  // Tasks the material states about the offering rather than about one product.
  // They belong to no passport, and filing them under an arbitrary product would
  // be an attribution nobody made.
  const loose = applications.filter((application) => application.product_id == null)
  const looseUncertainties = uncertainties.filter((item) => item.product_id == null)

  return (
    <div className="passports">
      <section className="passports__section" aria-labelledby="coverage-heading">
        <h3 id="coverage-heading">Что разобрано</h3>
        {coverage.length === 0 ? (
          <p className="knowledge-note">Ни один материал ещё не разбирали.</p>
        ) : (
          <>
            {blocked.length > 0 ? (
              <p className="passports__gate">
                Материалов, которые нельзя публиковать без человека: {blocked.length} из{' '}
                {coverage.length}.
              </p>
            ) : (
              <p className="passports__gate passports__gate--open">
                Все разобранные материалы прошли обе проверки: охват и состав паспорта.
              </p>
            )}
            <div className="passports__coverage">
              {coverage.map((report) => (
                <CoverageCard key={report.run_id} report={report} />
              ))}
            </div>
          </>
        )}
      </section>

      {loose.length > 0 ? (
        <section className="passports__section" aria-labelledby="offer-heading">
          <h3 id="offer-heading">Задачи по предложению в целом</h3>
          <ApplicationMap partnerId={partnerId} applications={loose} />
        </section>
      ) : null}

      <section className="passports__section" aria-labelledby="passports-heading">
        <h3 id="passports-heading">Паспорта изделий</h3>
        {passports.length === 0 ? (
          <p className="knowledge-note">Изделий пока нет.</p>
        ) : (
          <div className="passports__list">
            {passports.map((passport) => (
              <PassportCard
                key={passport.product.id}
                partnerId={partnerId}
                passport={passport}
              />
            ))}
          </div>
        )}
      </section>

      {looseUncertainties.length > 0 ? (
        <section className="passports__section" aria-labelledby="open-heading">
          <h3 id="open-heading">Неясности без привязки к изделию</h3>
          <p className="knowledge-note">
            Их нельзя отнести ни к одному изделию — именно это и делает их неясностями.
          </p>
          <UncertaintyList uncertainties={looseUncertainties} />
        </section>
      ) : null}
    </div>
  )
}
