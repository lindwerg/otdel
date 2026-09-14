import { render, screen, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import type { PageView } from '../../api/types'
import { exactSpan, unlocatedSpan } from '../../test/extraction'
import { PageSourceMap } from '../PageSourceMap'

const ORIGINAL_URL = '/api/partners/partner-a/materials/material-1/original#page=3'

function view(overrides: Partial<PageView> = {}): PageView {
  return {
    page_number: 3,
    width_pt: 595,
    height_pt: 842,
    rotation: 0,
    original_url: ORIGINAL_URL,
    diagram_interpretation: 'none',
    regions: [
      {
        region_id: 'region-table',
        ordinal: 0,
        kind: 'table',
        span: exactSpan(3, { x0: 50, y0: 620, x1: 460, y1: 710 }),
      },
    ],
    unplaced: [],
    ...overrides,
  }
}

function placedRegions(): HTMLElement[] {
  return within(
    screen.getByRole('list', { name: 'Области с известными координатами' }),
  ).getAllByRole('listitem')
}

describe('PageSourceMap', () => {
  it('says it is a schematic of positions and not a picture of the page', () => {
    render(<PageSourceMap view={view()} />)

    expect(screen.getByText(/Схема расположения областей/)).toBeTruthy()
    expect(screen.getByText(/не изображение страницы/)).toBeTruthy()
    // Nothing anywhere offers a rendering of the document itself.
    expect(document.body.textContent).not.toMatch(/так выглядит страница|предпросмотр/i)
  })

  it('converts PDF coordinates (y up from the bottom) into CSS percentages', () => {
    render(<PageSourceMap view={view()} />)

    const [rectangle] = placedRegions()
    // x0 = 50 of 595 pt wide.
    expect(rectangle.style.left).toBe('8.4%')
    // The box's upper edge is y1 = 710, measured from the bottom; from the top
    // of an 842 pt page that is 132 pt. Reading this the other way round would
    // mirror every highlight onto the wrong part of the document.
    expect(rectangle.style.top).toBe('15.68%')
    expect(rectangle.style.width).toBe('68.91%')
    expect(rectangle.style.height).toBe('10.69%')
  })

  it('lists a region without coordinates and draws no rectangle for it', () => {
    render(
      <PageSourceMap
        view={view({
          regions: [
            {
              region_id: 'region-ocr',
              ordinal: 1,
              kind: 'paragraph',
              span: unlocatedSpan(3, 'движок распознавания не возвращает координат слов'),
            },
          ],
        })}
      />,
    )

    const unplaced = within(screen.getByRole('list', { name: 'Области без координат' }))
    expect(
      unplaced.getByText(/движок распознавания не возвращает координат слов/),
    ).toBeTruthy()
    // Listed, never drawn: a rectangle here would be a claim about a position
    // nobody recorded.
    expect(screen.queryByRole('list', { name: 'Области с известными координатами' })).toBeNull()
    expect(document.querySelector('[data-region-id="region-ocr"].page-map__region')).toBeNull()
  })

  it('repeats the server’s reason for a region the endpoint itself could not place', () => {
    render(
      <PageSourceMap
        view={view({
          unplaced: [
            {
              region_id: 'region-note',
              ordinal: 2,
              kind: 'footnote',
              reason: 'область собрана из нескольких фрагментов, единый прямоугольник не определён',
            },
          ],
        })}
      />,
    )

    expect(screen.getByText(/единый прямоугольник не определён/)).toBeTruthy()
    expect(placedRegions()).toHaveLength(1)
    expect(document.querySelector('[data-region-id="region-note"].page-map__region')).toBeNull()
  })

  it('draws no map at all when the page size was never recorded, and says why', () => {
    render(<PageSourceMap view={view({ width_pt: null, height_pt: null })} />)

    expect(screen.getByText(/размер страницы не сохранён/)).toBeTruthy()
    expect(screen.getByText(/подставлять стандартный лист система не будет/)).toBeTruthy()
    expect(screen.queryByRole('list', { name: 'Области с известными координатами' })).toBeNull()
    // The regions are still accounted for — they are listed, with the reason.
    expect(
      within(screen.getByRole('list', { name: 'Области без координат' })).getAllByRole(
        'listitem',
      ),
    ).toHaveLength(1)
  })

  it('emphasises the chosen region in words, not by colour alone', () => {
    render(<PageSourceMap view={view()} highlightedRegionId="region-table" />)

    const [rectangle] = placedRegions()
    expect(rectangle.dataset.highlighted).toBe('true')
    expect(within(rectangle).getByText(/выбрано/)).toBeTruthy()
  })

  it('warns that a rotated page is shown in its stored orientation', () => {
    render(<PageSourceMap view={view({ rotation: 90 })} />)

    expect(screen.getByText(/координаты записаны до поворота/)).toBeTruthy()
  })

  it('keeps the real document one click away, because the map is not it', () => {
    render(<PageSourceMap view={view()} />)

    const link = screen.getByRole('link', { name: /Открыть оригинал, стр\. 3/ })
    expect(link.getAttribute('href')).toBe(ORIGINAL_URL)
  })
})
