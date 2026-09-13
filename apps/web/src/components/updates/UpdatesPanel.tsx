import { useCallback, useState } from 'react'
import {
  fetchVersionChanges,
  fetchVersionExport,
  reprocessMaterial,
  requestRefresh,
} from '../../api/client'
import type { ExportDocument, RefreshPlan, VersionChanges } from '../../api/types'
import { useAuth } from '../../auth/AuthContext'
import { useUpdates } from '../../hooks/useUpdates'
import { StatusMessage } from '../StatusMessage'
import { EventLog } from './EventLog'
import { RefreshStatusCard } from './RefreshStatusCard'
import { RetentionNotice } from './RetentionNotice'
import { VersionChangesView } from './VersionChangesView'

interface UpdatesPanelProps {
  partnerId: string
}

/**
 * Phase 1F — the cycle around the published version.
 *
 * Four things on one screen, because they answer one question between them:
 * *«новый материал пришёл — и что теперь?»* The refresh status says what is out
 * of date and why, the comparison says what the last new version changed, the
 * history says what happened, and the retention notice says what will be kept.
 *
 * A failed refresh does not take the screen away: what is shown stays what the
 * server last said, and the error appears above it. Losing the owner's place
 * over a two-second network blip helps nobody.
 */
export function UpdatesPanel({ partnerId }: UpdatesPanelProps) {
  const { runMutation, runRead } = useAuth()
  const { data, error, reload } = useUpdates(partnerId)
  const [plan, setPlan] = useState<RefreshPlan | null>(null)
  const [changes, setChanges] = useState<VersionChanges | null>(null)
  const [busy, setBusy] = useState(false)
  const [reprocessing, setReprocessing] = useState<string | null>(null)
  const [actionError, setActionError] = useState<string | null>(null)
  const [exported, setExported] = useState<ExportDocument | null>(null)

  const onRefresh = useCallback(async () => {
    setBusy(true)
    setActionError(null)
    try {
      const result = await runMutation((token) => requestRefresh(partnerId, token))
      setPlan(result)
      reload()
    } catch (err) {
      // The server's own reason beats anything this component could invent.
      setActionError(err instanceof Error ? err.message : 'Не удалось запросить обновление.')
    } finally {
      setBusy(false)
    }
  }, [partnerId, reload, runMutation])

  const onReprocess = useCallback(
    async (materialId: string) => {
      setReprocessing(materialId)
      setActionError(null)
      try {
        await runMutation((token) => reprocessMaterial(partnerId, materialId, token))
        reload()
      } catch (err) {
        setActionError(
          err instanceof Error ? err.message : 'Не удалось поставить материал на повторное чтение.',
        )
      } finally {
        setReprocessing(null)
      }
    },
    [partnerId, reload, runMutation],
  )

  const onShowChanges = useCallback(async () => {
    const versionId = data?.status.published?.id ?? data?.status.latest?.id
    if (!versionId) return
    setActionError(null)
    try {
      const result = await runRead(() => fetchVersionChanges(partnerId, versionId))
      setChanges(result)
    } catch (err) {
      setActionError(err instanceof Error ? err.message : 'Не удалось сравнить версии.')
    }
  }, [data, partnerId, runRead])

  const onExport = useCallback(async () => {
    const versionId = data?.status.published?.id
    if (!versionId) return
    setActionError(null)
    try {
      const document = await runRead(() => fetchVersionExport(partnerId, versionId))
      setExported(document)
    } catch (err) {
      setActionError(err instanceof Error ? err.message : 'Не удалось выгрузить версию.')
    }
  }, [data, partnerId, runRead])

  if (!data) {
    return (
      <section className="panel" aria-labelledby="updates-heading">
        <h2 id="updates-heading" className="section-heading">
          Обновления
        </h2>
        <StatusMessage tone={error ? 'error' : 'neutral'} onRetry={error ? reload : undefined}>
          {error ?? 'Загружаем состояние обновлений…'}
        </StatusMessage>
      </section>
    )
  }

  const publishedId = data.status.published?.id ?? null

  return (
    <section className="panel updates" aria-labelledby="updates-heading">
      <h2 id="updates-heading" className="section-heading">
        Обновления
      </h2>

      {error ? (
        <StatusMessage tone="warn" onRetry={reload}>
          {error}
        </StatusMessage>
      ) : null}

      <RefreshStatusCard
        status={data.status}
        plan={plan}
        busy={busy}
        actionError={actionError}
        onRefresh={onRefresh}
        onReprocess={onReprocess}
        reprocessing={reprocessing}
      />

      <div className="updates__actions">
        <button
          type="button"
          className="button button-outline"
          onClick={onShowChanges}
          disabled={!data.status.published && !data.status.latest}
          title={
            data.status.published || data.status.latest
              ? undefined
              : 'Сравнивать нечего: версий у партнёра пока нет'
          }
        >
          Показать изменения версии
        </button>
        <button
          type="button"
          className="button button-ghost"
          onClick={onExport}
          disabled={!publishedId}
          title={
            publishedId
              ? 'Выгрузить опубликованную версию с источниками и оговорками'
              : 'Выгружать нечего: опубликованной версии нет'
          }
        >
          Выгрузить опубликованную версию
        </button>
      </div>

      {exported ? (
        <section className="export" aria-labelledby="export-heading">
          <h3 id="export-heading" className="section-heading">
            Выгрузка версии {exported.manifest.version_number}
          </h3>
          <p className="knowledge-note">
            Формат: <code>{exported.manifest.schema}</code> · утверждений{' '}
            {exported.manifest.claims_total}, из них подтверждено источником{' '}
            {exported.manifest.claims_source_supported} · пробелов {exported.manifest.gaps_total}
          </p>
          <div className="export__disclosure">
            <p className="run__label">Оговорки, которые едут вместе с выгрузкой:</p>
            <ul aria-label="Оговорки выгрузки">
              {exported.manifest.disclosure.map((line) => (
                <li key={line}>{line}</li>
              ))}
            </ul>
          </div>
        </section>
      ) : null}

      {changes ? <VersionChangesView changes={changes} /> : null}

      <EventLog events={data.events} />
      <RetentionNotice policy={data.retention} />
    </section>
  )
}
