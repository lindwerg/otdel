import { useId } from 'react'
import type { BoundingBox, PageView, PlacedRegion, RegionKind } from '../api/types'
import { isHighlightableSpan, regionKindLabel, spanUnavailableReason } from '../lib/format'

interface PageSourceMapProps {
  view: PageView
  /**
   * The region to emphasise, if any. Emphasis is visual *and* textual — a
   * reader who cannot tell the colours apart still sees which one is meant.
   */
  highlightedRegionId?: string | null
}

/** A rectangle in CSS percentages of the frame: origin top-left, y counts down. */
interface CssRect {
  top: number
  left: number
  width: number
  height: number
}

interface PlacedOnMap {
  region: PlacedRegion
  rect: CssRect
}

/** A region the map cannot draw, with the reason it cannot. Listed, never drawn. */
interface NotOnMap {
  region_id: string
  ordinal: number
  kind: RegionKind
  reason: string
}

function round(value: number): number {
  return Math.round(value * 100) / 100
}

/**
 * PDF user space → CSS percentages.
 *
 * The two coordinate systems disagree about which way is up: a PDF measures y
 * from the bottom of the sheet upwards, CSS measures it from the top downwards.
 * So the top edge of a box is the distance from the top of the page to its
 * *upper* y — `height - y1` — and getting this backwards silently mirrors every
 * highlight, which looks plausible and points at the wrong part of the document.
 *
 * Returns `null` for a rectangle that cannot be shown (zero-sized, or entirely
 * outside the sheet): the caller then lists the region with a reason instead of
 * drawing a degenerate box.
 */
function toCssRect(bbox: BoundingBox, widthPt: number, heightPt: number): CssRect | null {
  if (widthPt <= 0 || heightPt <= 0) return null

  const left = Math.min(bbox.x0, bbox.x1)
  const right = Math.max(bbox.x0, bbox.x1)
  const bottom = Math.min(bbox.y0, bbox.y1)
  const top = Math.max(bbox.y0, bbox.y1)

  const clampedLeft = Math.max(0, Math.min(widthPt, left))
  const clampedRight = Math.max(0, Math.min(widthPt, right))
  const clampedBottom = Math.max(0, Math.min(heightPt, bottom))
  const clampedTop = Math.max(0, Math.min(heightPt, top))

  const width = clampedRight - clampedLeft
  const height = clampedTop - clampedBottom
  if (width <= 0 || height <= 0) return null

  return {
    top: round(((heightPt - clampedTop) / heightPt) * 100),
    left: round((clampedLeft / widthPt) * 100),
    width: round((width / widthPt) * 100),
    height: round((height / heightPt) * 100),
  }
}

/**
 * Split the server's regions into what can honestly be drawn and what cannot.
 *
 * The endpoint already separates the two, but a span that arrives without usable
 * coordinates is moved to the second list here rather than drawn somewhere
 * plausible. A rectangle on this map is a claim about where something is.
 */
function splitRegions(view: PageView): { placed: PlacedOnMap[]; notPlaced: NotOnMap[] } {
  const placed: PlacedOnMap[] = []
  const notPlaced: NotOnMap[] = view.unplaced.map((region) => ({
    region_id: region.region_id,
    ordinal: region.ordinal,
    kind: region.kind,
    reason: region.reason,
  }))

  for (const region of view.regions) {
    const rect =
      isHighlightableSpan(region.span) && region.span.bbox && view.width_pt && view.height_pt
        ? toCssRect(region.span.bbox, view.width_pt, view.height_pt)
        : null
    if (rect) {
      placed.push({ region, rect })
      continue
    }
    notPlaced.push({
      region_id: region.region_id,
      ordinal: region.ordinal,
      kind: region.kind,
      reason:
        spanUnavailableReason(region.span) ?? 'координаты области непригодны для показа',
    })
  }

  return { placed, notPlaced }
}

function UnplacedList({ regions }: { regions: NotOnMap[] }) {
  if (regions.length === 0) return null
  return (
    <div className="page-map__unplaced">
      <p className="page-map__unplaced-title">
        Не показаны на схеме — координаты неизвестны. Эти области существуют, но где они
        на листе, система не знает и не угадывает:
      </p>
      <ul className="page-map__unplaced-list" aria-label="Области без координат">
        {regions.map((region) => (
          <li key={region.region_id} data-region-id={region.region_id}>
            <span className="page-map__unplaced-kind">
              {regionKindLabel(region.kind)} №{region.ordinal}
            </span>{' '}
            {/* The server's own reason, verbatim. */}
            <span className="page-map__unplaced-reason">{region.reason}</span>
          </li>
        ))}
      </ul>
    </div>
  )
}

/**
 * Where the regions of one page sit on the sheet.
 *
 * This is a schematic of positions, not the page. Phase 1B stores no page
 * images, so there is nothing behind these rectangles and the interface says so
 * plainly instead of letting a frame full of boxes read as "here is your
 * document". Nothing here is a rendering of the original, and nothing is drawn
 * for a region whose coordinates were never recorded.
 */
export function PageSourceMap({ view, highlightedRegionId }: PageSourceMapProps) {
  const captionId = useId()
  const { placed, notPlaced } = splitRegions(view)
  const widthPt = view.width_pt
  const heightPt = view.height_pt

  if (widthPt === null || heightPt === null || widthPt <= 0 || heightPt <= 0) {
    // Without the sheet's size there is no frame to scale the rectangles into,
    // and a map drawn to an assumed A4 would be a guess presented as a diagram.
    return (
      <section className="page-map" aria-label={`Схема областей страницы ${view.page_number}`}>
        <p className="page-note">
          Схему расположения областей построить не из чего: размер страницы не сохранён,
          а подставлять стандартный лист система не будет. Ниже — что известно об областях.
        </p>
        <UnplacedList
          regions={[
            ...placed.map(({ region }) => ({
              region_id: region.region_id,
              ordinal: region.ordinal,
              kind: region.kind,
              reason: 'размер страницы неизвестен, масштабировать координаты не к чему',
            })),
            ...notPlaced,
          ]}
        />
      </section>
    )
  }

  return (
    <figure className="page-map" aria-labelledby={captionId}>
      <figcaption id={captionId} className="page-map__caption">
        Схема расположения областей на странице {view.page_number}. Это не изображение
        страницы: изображений страниц система не хранит. Показаны только прямоугольники,
        записанные при разборе, — по ним нельзя судить о том, как страница выглядит.
      </figcaption>

      <div
        className="page-map__frame"
        style={{ aspectRatio: `${widthPt} / ${heightPt}` }}
      >
        {placed.length === 0 ? (
          <p className="page-map__blank">Ни одной области с координатами.</p>
        ) : (
          <ul className="page-map__regions" aria-label="Области с известными координатами">
            {placed.map(({ region, rect }) => {
              const isHighlighted = region.region_id === highlightedRegionId
              return (
                <li
                  key={region.region_id}
                  className="page-map__region"
                  data-region-id={region.region_id}
                  data-kind={region.kind}
                  data-highlighted={isHighlighted ? 'true' : undefined}
                  style={{
                    top: `${rect.top}%`,
                    left: `${rect.left}%`,
                    width: `${rect.width}%`,
                    height: `${rect.height}%`,
                  }}
                >
                  <span className="page-map__region-label">
                    {regionKindLabel(region.kind)} №{region.ordinal}
                    {/* Emphasis is never colour alone: the chosen region says so. */}
                    {isHighlighted ? <span className="page-map__chosen"> — выбрано</span> : null}
                  </span>
                </li>
              )
            })}
          </ul>
        )}
      </div>

      <p className="page-map__facts">
        Размер страницы: {Math.round(widthPt)}×{Math.round(heightPt)} pt.
        {view.rotation !== 0 ? (
          <>
            {' '}
            Страница повёрнута на {view.rotation}°: координаты записаны до поворота, поэтому
            схема показывает их в исходной ориентации, а не так, как страницу покажет
            просмотрщик.
          </>
        ) : null}
      </p>

      <p className="page-map__original">
        {/* The schematic is not the document, so the document stays one click away. */}
        <a href={view.original_url} target="_blank" rel="noreferrer">
          Открыть оригинал, стр. {view.page_number}
        </a>
      </p>

      <UnplacedList regions={notPlaced} />
    </figure>
  )
}
