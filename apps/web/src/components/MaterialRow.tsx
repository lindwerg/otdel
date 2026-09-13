import { useState } from 'react'
import type { Material } from '../api/types'
import { originalMaterialUrl } from '../api/client'
import {
  extractionCountsLine,
  formatBytes,
  formatDateTime,
  materialStatusPresentation,
  pagesNeedingAttention,
  RETRYABLE_MATERIAL_STATUSES,
} from '../lib/format'
import { MaterialPages } from './MaterialPages'

interface MaterialRowProps {
  material: Material
  onRetry: (material: Material) => void
  retrying: boolean
  retryError: string | null
  /** Called after a single page was re-queued, so the summary can be refreshed. */
  onPageChanged: () => void
}

function extensionLabel(filename: string, mediaType: string): string {
  const dot = filename.lastIndexOf('.')
  if (dot >= 0 && dot < filename.length - 1) {
    return filename.slice(dot + 1, dot + 4).toUpperCase()
  }
  if (mediaType.includes('pdf')) return 'PDF'
  if (mediaType.includes('png')) return 'PNG'
  if (mediaType.includes('jpeg') || mediaType.includes('jpg')) return 'JPG'
  return '·'
}

export function MaterialRow({
  material,
  onRetry,
  retrying,
  retryError,
  onPageChanged,
}: MaterialRowProps) {
  const presentation = materialStatusPresentation(material.status)
  const hint = material.error || presentation.defaultHint
  const canRetry = RETRYABLE_MATERIAL_STATUSES.includes(material.status)
  const summary = material.extraction
  const [pagesOpen, setPagesOpen] = useState(false)

  return (
    <li className="material-item" data-state={material.status}>
      <span className="material-icon" aria-hidden="true">
        {extensionLabel(material.filename, material.media_type)}
      </span>
      <div className="material-body">
        <strong>{material.filename}</strong>
        <span className="material-meta">
          {formatBytes(material.size_bytes)}
          {material.page_count != null ? ` · ${material.page_count} стр.` : ''} · загружен{' '}
          {formatDateTime(material.created_at)}
        </span>
        <span
          className="material-outcome"
          data-tone={presentation.tone === 'progress' ? undefined : presentation.tone}
        >
          <strong>{presentation.label}</strong>
          {hint ? ` — ${hint}` : ''}
        </span>

        {/* Counts, not a percentage: how many pages ended up in which state is a
            fact; "62% обработано" would be an estimate of understanding that
            nothing here can measure. */}
        {summary ? (
          <span className="material-pages-line">
            {extractionCountsLine(summary)}
            {pagesNeedingAttention(summary) > 0
              ? ` Требуют внимания: ${pagesNeedingAttention(summary)}.`
              : ''}
          </span>
        ) : null}

        <div className="material-actions">
          {summary ? (
            <button
              type="button"
              className="button button-ghost button-small"
              aria-expanded={pagesOpen}
              onClick={() => setPagesOpen((open) => !open)}
            >
              {pagesOpen ? 'Скрыть страницы' : 'Показать страницы'}
            </button>
          ) : null}
          <a
            className="button button-ghost button-small"
            href={originalMaterialUrl(material.partner_id, material.id)}
            target="_blank"
            rel="noreferrer"
          >
            Открыть оригинал
          </a>
          {canRetry ? (
            <button
              type="button"
              className="button button-outline button-small"
              onClick={() => onRetry(material)}
              disabled={retrying}
            >
              {retrying ? 'Повторяем…' : 'Обработать заново'}
            </button>
          ) : null}
        </div>

        {retryError ? (
          <p className="field-error" role="alert">
            {retryError}
          </p>
        ) : null}

        {pagesOpen && summary ? (
          <MaterialPages material={material} onPageChanged={onPageChanged} />
        ) : null}
      </div>
    </li>
  )
}
