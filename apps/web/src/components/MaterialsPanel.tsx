import { useState } from 'react'
import { retryMaterial } from '../api/client'
import type { Material } from '../api/types'
import { useAuth } from '../auth/AuthContext'
import { useMaterials } from '../hooks/useMaterials'
import { MaterialRow } from './MaterialRow'
import { MaterialUploadDialog } from './MaterialUploadDialog'
import { StatusMessage } from './StatusMessage'

interface MaterialsPanelProps {
  partnerId: string
  partnerName: string
}

export function MaterialsPanel({ partnerId, partnerName }: MaterialsPanelProps) {
  const { runMutation } = useAuth()
  const { materials, loadError, reload, upsert } = useMaterials(partnerId)
  const [uploadOpen, setUploadOpen] = useState(false)
  const [retryingId, setRetryingId] = useState<string | null>(null)
  const [retryErrors, setRetryErrors] = useState<Record<string, string>>({})

  async function handleRetry(material: Material) {
    setRetryingId(material.id)
    setRetryErrors((prev) => {
      const next = { ...prev }
      delete next[material.id]
      return next
    })
    try {
      const updated = await runMutation((token) => retryMaterial(partnerId, material.id, token))
      upsert(updated)
    } catch (err) {
      const message = err instanceof Error ? err.message : 'Не удалось повторить обработку.'
      setRetryErrors((prev) => ({ ...prev, [material.id]: message }))
    } finally {
      setRetryingId(null)
    }
  }

  return (
    <section aria-labelledby="materials-heading">
      <div className="section-heading">
        <h2 id="materials-heading">Материалы</h2>
      </div>

      <div className="upload-cta">
        <p>Каталоги, презентации и фотографии для этого партнёра.</p>
        <button type="button" className="button button-primary" onClick={() => setUploadOpen(true)}>
          Загрузить материалы
        </button>
      </div>

      {materials === null && !loadError ? (
        <StatusMessage>Загружаем список материалов…</StatusMessage>
      ) : null}

      {loadError ? (
        <StatusMessage tone="error" onRetry={reload}>
          {loadError}
        </StatusMessage>
      ) : null}

      {materials && materials.length === 0 ? (
        <div className="empty-state">
          <img src="/otto/welcome.png" alt="" aria-hidden="true" width="120" height="120" />
          <h2>Материалов пока нет</h2>
          <p>Загрузите первый файл — техническую документацию, презентацию или фотографию.</p>
        </div>
      ) : null}

      {materials && materials.length > 0 ? (
        <ul className="material-list">
          {materials.map((material) => (
            <MaterialRow
              key={material.id}
              material={material}
              onRetry={handleRetry}
              retrying={retryingId === material.id}
              retryError={retryErrors[material.id] ?? null}
            />
          ))}
        </ul>
      ) : null}

      <MaterialUploadDialog
        open={uploadOpen}
        partnerId={partnerId}
        partnerName={partnerName}
        onClose={() => setUploadOpen(false)}
        onUploaded={upsert}
      />
    </section>
  )
}
