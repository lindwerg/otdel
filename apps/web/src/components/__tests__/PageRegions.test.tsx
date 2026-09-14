import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import type { PageRegion } from '../../api/types'
import { exactSpan, loadTableRegion, unlocatedSpan } from '../../test/extraction'
import { PageRegions } from '../PageRegions'

const HEADER_TEXT = 'безопасная рабочая нагрузка (Н)'

function paragraphRegion(overrides: Partial<PageRegion> = {}): PageRegion {
  return {
    id: 'region-paragraph',
    page_id: 'page-3',
    page_number: 3,
    ordinal: 1,
    kind: 'paragraph',
    text: 'Нагрузки указаны для схемы опирания по двум опорам.',
    source: 'ocr',
    bbox: null,
    row_count: null,
    column_count: null,
    cells: [],
    span: unlocatedSpan(3, 'движок распознавания не возвращает координат слов'),
    ...overrides,
  }
}

/** The data cells of one body row: header cells are `<th>` and are excluded. */
function dataCells(rowIndex: number): HTMLElement[] {
  const rows = within(screen.getByRole('table')).getAllByRole('row')
  return within(rows[rowIndex]).getAllByRole('cell')
}

describe('PageRegions', () => {
  it('renders a column label as a header and marks it as not a value', () => {
    render(<PageRegions regions={[loadTableRegion()]} />)

    const header = screen.getByRole('columnheader', { name: /безопасная рабочая нагрузка/ })
    expect(within(header).getByText(HEADER_TEXT)).toBeTruthy()
    expect(within(header).getByText('не значение')).toBeTruthy()
    expect(within(header).getByText(/это заголовок таблицы, а не значение/)).toBeTruthy()

    // The decisive assertion: this string is never offered anywhere a value
    // would be read from. It was published as a characteristic once.
    for (const cell of screen.getAllByRole('cell')) {
      expect(cell.textContent).not.toContain(HEADER_TEXT)
    }
  })

  it('renders a row label as a row header rather than as a value', () => {
    render(<PageRegions regions={[loadTableRegion()]} />)

    const rowHeader = screen.getByRole('rowheader', { name: /BP21/ })
    expect(rowHeader.getAttribute('scope')).toBe('row')
    expect(within(rowHeader).getByText('не значение')).toBeTruthy()
  })

  it('shows product, property, value, unit and condition as separate pieces', () => {
    render(<PageRegions regions={[loadTableRegion()]} />)

    // Row 1 of the table body: BP21. Its second data cell carries the load.
    const loadCell = dataCells(1)[1]

    const value = within(loadCell).getByText('4860')
    expect(value.textContent).toBe('4860')

    expect(within(loadCell).getByText('изделие')).toBeTruthy()
    expect(within(loadCell).getByText('BP21')).toBeTruthy()
    expect(within(loadCell).getByText('характеристика')).toBeTruthy()

    const property = within(loadCell).getByText('безопасная рабочая нагрузка')
    expect(property).not.toBe(value)

    expect(within(loadCell).getByText('единица')).toBeTruthy()
    // Twice on purpose: next to the number as it should be read, and again in
    // the context, where it says the unit was written in the column header.
    expect(within(loadCell).getAllByText('Н')).toHaveLength(2)
    // Both the property and the unit were read in the column header, and each
    // says so for itself.
    expect(within(loadCell).getAllByText('написано в заголовке столбца')).toHaveLength(2)

    expect(within(loadCell).getByText('условие')).toBeTruthy()
    expect(within(loadCell).getByText('при опирании на две опоры')).toBeTruthy()

    // A fully attributed value carries no verdict note: there is nothing to warn about.
    expect(within(loadCell).queryByText(/не значение|неоднозначно/)).toBeNull()
  })

  it('marks context carried across a merged cell as inferred', () => {
    render(<PageRegions regions={[loadTableRegion()]} />)

    const lengthCell = dataCells(2)[0]
    expect(within(lengthCell).getByText(/перенесено из объединённой ячейки/)).toBeTruthy()
    expect(within(lengthCell).getByText(/это предположение, а не прочитанное/)).toBeTruthy()
    expect(within(lengthCell).getByText(/неоднозначно/)).toBeTruthy()
    expect(
      within(lengthCell).getByText(/контекст перенесён из объединённой ячейки/),
    ).toBeTruthy()
  })

  it('leaves a value the source never printed blank, and says why', () => {
    render(<PageRegions regions={[loadTableRegion()]} />)

    const blankCell = dataCells(2)[1]
    const blank = within(blankCell).getByLabelText('значение не указано в источнике')
    // Blank stays blank: not a zero, not a dash.
    expect(blank.textContent).toBe('')
    expect(blankCell.querySelector('.cell-line')?.textContent?.trim()).toBe('')
    expect(within(blankCell).getByText(/в источнике ячейка пуста/)).toBeTruthy()
  })

  it('repeats the server’s reason instead of inventing coordinates', () => {
    render(<PageRegions regions={[paragraphRegion()]} />)

    expect(
      screen.getByText('движок распознавания не возвращает координат слов'),
    ).toBeTruthy()
  })

  it('offers "show on the map" only for a region that has coordinates', async () => {
    const user = userEvent.setup()
    const onHighlight = vi.fn()
    render(
      <PageRegions
        regions={[loadTableRegion(), paragraphRegion()]}
        onHighlight={onHighlight}
      />,
    )

    const items = screen.getAllByRole('listitem')
    expect(within(items[1]).queryByRole('button')).toBeNull()

    await user.click(within(items[0]).getByRole('button', { name: 'Показать на схеме' }))
    expect(onHighlight).toHaveBeenCalledWith('region-table')
  })

  it('states which region is shown on the map without relying on colour', () => {
    render(
      <PageRegions
        regions={[loadTableRegion({ span: exactSpan(3, { x0: 1, y0: 2, x1: 3, y1: 4 }) })]}
        highlightedRegionId="region-table"
        onHighlight={() => {}}
      />,
    )

    const button = screen.getByRole('button', { name: 'Показано на схеме' })
    expect(button.getAttribute('aria-pressed')).toBe('true')
  })
})
