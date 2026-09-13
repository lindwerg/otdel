import type { PageRegion, TableCell } from '../api/types'

interface PageRegionsProps {
  regions: PageRegion[]
}

const REGION_LABEL: Record<PageRegion['kind'], string> = {
  heading: 'Заголовок',
  paragraph: 'Абзац',
  footnote: 'Сноска',
  table: 'Таблица',
}

/**
 * Where a region sits on the page, in the document's own units.
 *
 * Shown only when the server recorded coordinates. A region extracted from
 * recognised text has none — the engine returns text, not positions — and the
 * interface says so instead of drawing a box somewhere plausible.
 */
function positionLabel(region: PageRegion): string {
  if (!region.bbox) return 'координаты неизвестны'
  const { x0, y0, x1, y1 } = region.bbox
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

function TableRegion({ region }: { region: PageRegion }) {
  const rows = toRows(region.cells)
  if (rows.length === 0) return null
  const [first, ...rest] = rows
  const hasHeader = first.some((cell) => cell.is_header)

  return (
    <div className="page-table-wrap">
      <table className="page-table">
        <caption className="visually-hidden">
          Таблица со страницы {region.page_number}: {region.row_count} строк,{' '}
          {region.column_count} столбцов
        </caption>
        {hasHeader ? (
          <thead>
            <tr>
              {first.map((cell) => (
                <th key={cell.id} scope="col">
                  {cell.raw_text || '—'}
                </th>
              ))}
            </tr>
          </thead>
        ) : null}
        <tbody>
          {(hasHeader ? rest : rows).map((row) => (
            <tr key={row[0]?.id ?? row.length}>
              {row.map((cell) => (
                <td key={cell.id} data-kind={cell.value_kind}>
                  {/* A blank cell is shown as blank on purpose: the source did
                      not print a value, and a dash or a zero here would be the
                      interface inventing one. */}
                  {cell.raw_text ? (
                    <>
                      <span className="cell-value">{cell.raw_text}</span>
                      {cell.unit ? <span className="cell-unit"> {cell.unit}</span> : null}
                    </>
                  ) : (
                    <span className="cell-empty" aria-label="значение не указано в источнике" />
                  )}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

/**
 * The structural regions of a page: what was found, what kind it is, and — when
 * it is known — where on the page it sits.
 */
export function PageRegions({ regions }: PageRegionsProps) {
  if (regions.length === 0) {
    return <p className="page-note">Структурных областей на этой странице не сохранено.</p>
  }

  return (
    <ol className="region-list">
      {regions.map((region) => (
        <li key={region.id} className="region-item" data-kind={region.kind}>
          <div className="region-head">
            <span className="region-kind">{REGION_LABEL[region.kind] ?? region.kind}</span>
            <span className="region-place">{positionLabel(region)}</span>
          </div>
          {region.kind === 'table' ? (
            <TableRegion region={region} />
          ) : (
            <p className="region-text">{region.text}</p>
          )}
        </li>
      ))}
    </ol>
  )
}
