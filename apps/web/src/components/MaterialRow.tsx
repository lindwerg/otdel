import type { Material } from '../api/types'
import { originalMaterialUrl } from '../api/client'
import { formatBytes, formatDateTime, materialStatusPresentation, RETRYABLE_MATERIAL_STATUSES } from '../lib/format'

interface MaterialRowProps {
  material: Material
  onRetry: (material: Material) => void
  retrying: boolean
  retryError: string | null
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

export function MaterialRow({ material, onRetry, retrying, retryError }: MaterialRowProps) {
  const presentation = materialStatusPresentation(material.status)
  const hint = material.error || presentation.defaultHint
  const canRetry = RETRYABLE_MATERIAL_STATUSES.includes(material.status)

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
        <span className="material-outcome" data-tone={presentation.tone === 'progress' ? undefined : presentation.tone}>
          <strong>{presentation.label}</strong>
          {hint ? ` — ${hint}` : ''}
        </span>
        {canRetry ? (
          <div className="material-actions">
            <button
              type="button"
              className="button button-outline button-small"
              onClick={() => onRetry(material)}
              disabled={retrying}
            >
              {retrying ? 'Повторяем…' : 'Повторить обработку'}
            </button>
            <a
              className="button button-ghost button-small"
              href={originalMaterialUrl(material.partner_id, material.id)}
              target="_blank"
              rel="noreferrer"
            >
              Открыть оригинал
            </a>
          </div>
        ) : (
          <div className="material-actions">
            <a
              className="button button-ghost button-small"
              href={originalMaterialUrl(material.partner_id, material.id)}
              target="_blank"
              rel="noreferrer"
            >
              Открыть оригинал
            </a>
          </div>
        )}
        {retryError ? (
          <p className="field-error" role="alert">
            {retryError}
          </p>
        ) : null}
      </div>
    </li>
  )
}
