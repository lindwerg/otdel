import { useEffect, useId, useRef, useState, type ChangeEvent } from 'react'
import { uploadMaterial } from '../api/client'
import type { Material } from '../api/types'
import { SessionExpiredError, useAuth } from '../auth/AuthContext'
import { formatBytes } from '../lib/format'
import { validateFileSize } from '../lib/validation'
import { Dialog } from './Dialog'

interface UploadRow {
  key: string
  file: File
  status: 'pending' | 'uploading' | 'done' | 'error'
  /** 0-100 when the browser reports a computable length; null otherwise (no fake percentage is shown then). */
  percent: number | null
  message: string | null
}

interface MaterialUploadDialogProps {
  open: boolean
  partnerId: string
  partnerName: string
  onClose: () => void
  onUploaded: (material: Material) => void
}

let rowKeySeq = 0

export function MaterialUploadDialog({
  open,
  partnerId,
  partnerName,
  onClose,
  onUploaded,
}: MaterialUploadDialogProps) {
  const { runMutation } = useAuth()
  const titleId = useId()
  const inputId = useId()
  const fileInputRef = useRef<HTMLInputElement>(null)
  const [rows, setRows] = useState<UploadRow[]>([])
  const busyRef = useRef(false)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    if (!open) {
      setRows([])
      if (fileInputRef.current) fileInputRef.current.value = ''
    }
  }, [open])

  function updateRow(key: string, patch: Partial<UploadRow>) {
    setRows((prev) => prev.map((row) => (row.key === key ? { ...row, ...patch } : row)))
  }

  async function runQueue(queue: UploadRow[]) {
    busyRef.current = true
    setBusy(true)
    for (const row of queue) {
      if (row.status !== 'pending') continue
      updateRow(row.key, { status: 'uploading', percent: null })
      try {
        // One multipart request per file, sequentially — matches the
        // documented endpoint (`multipart file` — one file per request).
        const material = await runMutation((token) =>
          uploadMaterial(partnerId, row.file, token, (progress) => {
            const percent = progress.total > 0 ? Math.round((progress.loaded / progress.total) * 100) : null
            updateRow(row.key, { percent })
          }),
        )
        updateRow(row.key, { status: 'done', percent: 100, message: null })
        onUploaded(material)
      } catch (err) {
        const message = err instanceof Error ? err.message : 'Не удалось загрузить файл.'
        updateRow(row.key, { status: 'error', message })
        if (err instanceof SessionExpiredError) {
          // The session-expired overlay takes over above this dialog; the
          // remaining queued files are marked (not silently dropped) rather
          // than retried blindly once a new password is entered.
          const remainingIndex = queue.indexOf(row) + 1
          for (const remaining of queue.slice(remainingIndex)) {
            if (remaining.status === 'pending') {
              updateRow(remaining.key, { status: 'error', message })
            }
          }
          break
        }
      }
    }
    busyRef.current = false
    setBusy(false)
  }

  function handleFileChange(event: ChangeEvent<HTMLInputElement>) {
    const files = event.target.files
    if (!files || files.length === 0) return
    const newRows: UploadRow[] = Array.from(files).map((file) => {
      const sizeError = validateFileSize(file)
      rowKeySeq += 1
      return {
        key: `${rowKeySeq}-${file.name}`,
        file,
        status: sizeError ? 'error' : 'pending',
        percent: null,
        message: sizeError,
      }
    })
    setRows((prev) => [...prev, ...newRows])
    event.target.value = ''
    void runQueue(newRows)
  }

  function handleRequestClose() {
    if (busyRef.current) return
    onClose()
  }

  return (
    <Dialog open={open} onClose={handleRequestClose} labelledBy={titleId} preventClose={busy}>
      <h2 id={titleId}>Материалы партнёра</h2>
      <p className="dialog-subtle">Загрузка для: {partnerName}</p>
      <div className="upload-area">
        <label htmlFor={inputId}>Файлы (PDF, PNG, JPEG)</label>
        <input
          ref={fileInputRef}
          id={inputId}
          type="file"
          multiple
          accept=".pdf,image/png,image/jpeg"
          onChange={handleFileChange}
          disabled={busy}
        />
        <p>Каждый файл до {formatBytes(25 * 1024 * 1024)}. Файлы отправляются по одному, по очереди.</p>
        {rows.length > 0 ? (
          <ul className="file-list" aria-live="polite">
            {rows.map((row) => (
              <li key={row.key} data-tone={row.status === 'error' ? 'error' : row.status === 'done' ? 'success' : undefined}>
                <span>
                  {row.file.name} · {formatBytes(row.file.size)}
                </span>
                <span className="file-status">
                  {row.status === 'pending' && 'В очереди'}
                  {row.status === 'uploading' && (row.percent != null ? `Загружаем… ${row.percent}%` : 'Загружаем…')}
                  {row.status === 'done' && 'Загружено'}
                  {row.status === 'error' && (row.message ?? 'Ошибка загрузки')}
                </span>
              </li>
            ))}
          </ul>
        ) : null}
      </div>
      <div className="dialog-actions">
        <button type="button" className="button button-primary" onClick={handleRequestClose} disabled={busy}>
          {busy ? 'Идёт загрузка…' : 'Готово'}
        </button>
      </div>
    </Dialog>
  )
}
