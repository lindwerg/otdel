import type { ReactNode } from 'react'
import type {
  CellVerdict,
  ConditionRef,
  ContextOrigin,
  PageRegion,
  TableCell,
} from '../api/types'
import {
  cellUsabilityPresentation,
  contextOriginLabel,
  isHighlightableSpan,
  isInferredOrigin,
  reasonLabel,
  regionKindLabel,
  spanUnavailableReason,
} from '../lib/format'

interface PageRegionsProps {
  regions: PageRegion[]
  /** The region currently emphasised on the page map, when there is one. */
  highlightedRegionId?: string | null
  /** Given only when a map is on screen: without one there is nowhere to show. */
  onHighlight?: (regionId: string) => void
}

/**
 * Where a region sits on the page, in the document's own units.
 *
 * Shown only when the server recorded coordinates. A region extracted from
 * recognised text has none — the engine returns text, not positions — and the
 * interface repeats the server's own reason instead of drawing a box somewhere
 * plausible.
 */
function positionLabel(region: PageRegion): string {
  const reason = spanUnavailableReason(region.span)
  if (reason !== null) return reason
  const bbox = region.span.bbox
  if (!bbox) return 'координаты неизвестны'
  const { x0, y0, x1, y1 } = bbox
  return `x ${Math.round(x0)}–${Math.round(x1)}, y ${Math.round(y0)}–${Math.round(y1)} pt`
}

/** Group cells into rows, keeping the server's row/column indices. */
function toRows(cells: TableCell[]): TableCell[][] {
  const rows = new Map<number, TableCell[]>()
  for (const cell of cells) {
    const row = rows.get(cell.row_index) ?? []
    row.push(cell)
    rows.set(cell.row_index, row)
  }
  return [...rows.entries()]
    .sort(([a], [b]) => a - b)
    .map(([, row]) => row.sort((a, b) => a.column_index - b.column_index))
}

/**
 * The verdict on one cell, in words.
 *
 * Rendered for everything except `usable`, and worded so that a heading reads as
 * a heading rather than as a broken value: `unusable` says «не значение», never
 * «ошибка». The reasons are the server's, translated to sentences — a reader
 * cannot act on `header_is_not_a_value`.
 */
function VerdictNote({ verdict }: { verdict: CellVerdict }) {
  const presentation = cellUsabilityPresentation(verdict.usability)
  return (
    <p className="cell-verdict" data-tone={presentation.tone} data-usability={verdict.usability}>
      <span className="cell-verdict__label">{presentation.label}</span>
      <span className="cell-verdict__reasons">
        {verdict.reasons.length > 0
          ? `: ${verdict.reasons.map(reasonLabel).join('; ')}`
          : ': причина сервером не указана'}
      </span>
    </p>
  )
}

/**
 * One piece of context, with where it was written.
 *
 * The origin is never dropped. Context carried across a merged (blank) cell is
 * additionally called a guess in so many words, because "BP21" inherited from
 * the row above and "BP21" printed in the row itself are not the same claim.
 */
function ContextRow({
  label,
  text,
  origin,
}: {
  label: string
  text: string
  origin: ContextOrigin
}) {
  const inferred = isInferredOrigin(origin)
  return (
    <div className="cell-context__pair">
      <dt className="cell-context__term">{label}</dt>
      <dd className="cell-context__value" data-inferred={inferred ? 'true' : undefined}>
        <span className="cell-context__text">{text}</span>{' '}
        <span className="cell-context__origin">{contextOriginLabel(origin)}</span>
        {inferred ? (
          <span className="cell-context__inferred"> — это предположение, а не прочитанное</span>
        ) : null}
      </dd>
    </div>
  )
}

function ConditionRows({ conditions }: { conditions: ConditionRef[] }): ReactNode {
  return conditions.map((condition, index) => (
    <div className="cell-context__pair" key={`${condition.marker ?? 'condition'}-${index}`}>
      <dt className="cell-context__term">условие</dt>
      <dd className="cell-context__value">
        {condition.marker ? (
          <span className="cell-context__marker">{condition.marker} </span>
        ) : null}
        <span className="cell-context__text">{condition.text}</span>
      </dd>
    </div>
  ))
}

/**
 * What a cell's number actually refers to, piece by piece.
 *
 * Product identity, property, unit and conditions are shown as separate entries
 * on purpose: they are different questions with different evidence, and merging
 * them into one line is exactly how a column label («безопасная рабочая нагрузка
 * (Н)») ended up published as a characteristic. Pieces the source never stated
 * are simply absent — the verdict above says which ones and why.
 */
function CellContext({ cell }: { cell: TableCell }) {
  const context = cell.structural_context
  const hasAnything =
    context.subject !== null ||
    context.property !== null ||
    context.unit !== null ||
    context.conditions.length > 0
  if (!hasAnything) return null

  return (
    <dl className="cell-context">
      {context.subject ? (
        <ContextRow
          label="изделие"
          text={context.subject.text}
          origin={context.subject.origin}
        />
      ) : null}
      {context.property ? (
        <ContextRow
          label="характеристика"
          text={context.property.text}
          origin={context.property.origin}
        />
      ) : null}
      {context.unit ? (
        <ContextRow
          label="единица"
          text={context.unit.unit}
          origin={context.unit.origin}
        />
      ) : null}
      <ConditionRows conditions={context.conditions} />
    </dl>
  )
}

/** The verbatim fragment, exactly as the source printed it — or a visible blank. */
function CellText({ cell }: { cell: TableCell }) {
  // A blank cell is shown as blank on purpose: the source did not print a value,
  // and a dash or a zero here would be the interface inventing one.
  if (!cell.raw_text) {
    return <span className="cell-empty" aria-label="значение не указано в источнике" />
  }
  return <span className="cell-value">{cell.raw_text}</span>
}

/**
 * One cell, as the grid recorded it.
 *
 * A header is rendered as a real `<th>` with a scope, because that is what it is
 * — in the accessibility tree as well as on screen. Nothing that the server
 * called a header is ever placed where a value would go.
 */
function CellNode({ cell }: { cell: TableCell }) {
  const usability = cell.verdict.usability

  if (cell.role === 'column_header' || cell.role === 'row_header') {
    return (
      <th
        scope={cell.role === 'column_header' ? 'col' : 'row'}
        data-role={cell.role}
        data-usability={usability}
      >
        <CellText cell={cell} />
        {usability !== 'usable' ? <VerdictNote verdict={cell.verdict} /> : null}
      </th>
    )
  }

  const unit = cell.structural_context.unit
  return (
    <td data-kind={cell.value_kind} data-role="data" data-usability={usability}>
      <span className="cell-line">
        <CellText cell={cell} />
        {/* The unit is appended only where the source wrote one somewhere; the
            context below says exactly where. */}
        {unit ? <span className="cell-unit"> {unit.unit}</span> : null}
      </span>
      {usability !== 'usable' ? <VerdictNote verdict={cell.verdict} /> : null}
      <CellContext cell={cell} />
    </td>
  )
}

function TableRegion({ region }: { region: PageRegion }) {
  const rows = toRows(region.cells)
  if (rows.length === 0) return null

  // The header band is every leading row that carries column labels. A two-row
  // header is still a header: treating its second row as data is how a unit line
  // («кН») turns into a measurement.
  let headerCount = 0
  while (
    headerCount < rows.length &&
    rows[headerCount].some((cell) => cell.role === 'column_header')
  ) {
    headerCount += 1
  }
  const headerRows = rows.slice(0, headerCount)
  const bodyRows = rows.slice(headerCount)

  return (
    <div className="page-table-wrap">
      <table className="page-table">
        <caption className="visually-hidden">
          Таблица со страницы {region.page_number}: {region.row_count} строк,{' '}
          {region.column_count} столбцов
        </caption>
        {headerRows.length > 0 ? (
          <thead>
            {headerRows.map((row) => (
              <tr key={row[0]?.id ?? `header-${row.length}`}>
                {row.map((cell) => (
                  <CellNode key={cell.id} cell={cell} />
                ))}
              </tr>
            ))}
          </thead>
        ) : null}
        {bodyRows.length > 0 ? (
          <tbody>
            {bodyRows.map((row) => (
              <tr key={row[0]?.id ?? `row-${row.length}`}>
                {row.map((cell) => (
                  <CellNode key={cell.id} cell={cell} />
                ))}
              </tr>
            ))}
          </tbody>
        ) : null}
      </table>
    </div>
  )
}

/**
 * The structural regions of a page: what was found, what kind it is, and — when
 * it is known — where on the page it sits.
 */
export function PageRegions({ regions, highlightedRegionId, onHighlight }: PageRegionsProps) {
  if (regions.length === 0) {
    return <p className="page-note">Структурных областей на этой странице не сохранено.</p>
  }

  return (
    <ol className="region-list">
      {regions.map((region) => {
        const isHighlighted = region.id === highlightedRegionId
        // No button for a region with no coordinates: it would point at nothing.
        const canShow = onHighlight !== undefined && isHighlightableSpan(region.span)
        return (
          <li
            key={region.id}
            className="region-item"
            data-kind={region.kind}
            data-highlighted={isHighlighted ? 'true' : undefined}
          >
            <div className="region-head">
              <span className="region-kind">{regionKindLabel(region.kind)}</span>
              <span className="region-place">{positionLabel(region)}</span>
              {canShow ? (
                <button
                  type="button"
                  className="button button-ghost button-small"
                  aria-pressed={isHighlighted}
                  onClick={() => onHighlight?.(region.id)}
                >
                  {isHighlighted ? 'Показано на схеме' : 'Показать на схеме'}
                </button>
              ) : null}
            </div>
            {region.kind === 'table' ? (
              <TableRegion region={region} />
            ) : (
              <p className="region-text">{region.text}</p>
            )}
          </li>
        )
      })}
    </ol>
  )
}
