import { useCallback, useEffect, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { ApiError, createPartner, getPartner, listPartners, updatePartner } from '../api/client'
import type { Partner } from '../api/types'
import { useAuth } from '../auth/AuthContext'
import { KnowledgePanel } from '../components/knowledge/KnowledgePanel'
import { MaterialsPanel } from '../components/MaterialsPanel'
import { PartnerFormDialog } from '../components/PartnerFormDialog'
import { PartnerSidebar } from '../components/PartnerSidebar'
import { ResearchPanel } from '../components/research/ResearchPanel'
import { StatusMessage } from '../components/StatusMessage'

/** Sections of the partner card that exist today. Knowledge is phase 1C and
 *  research phase 1D; versions and history arrive with 1E/1F and are deliberately
 *  absent rather than stubbed. */
const TABS = [
  { id: 'materials', label: 'Материалы' },
  { id: 'knowledge', label: 'Знания' },
  { id: 'research', label: 'Исследование' },
] as const

type TabId = (typeof TABS)[number]['id']

export function WorkspacePage() {
  const { logout, runMutation, runRead } = useAuth()
  const [searchParams, setSearchParams] = useSearchParams()
  const partnerId = searchParams.get('partner_id')
  // The open section lives in the URL next to the partner, so a reload — or a
  // link someone pasted — reopens what they were looking at.
  const tabParam = searchParams.get('tab')
  const activeTab: TabId = TABS.some((tab) => tab.id === tabParam)
    ? (tabParam as TabId)
    : 'materials'

  function selectTab(tab: TabId) {
    const next = new URLSearchParams(searchParams)
    if (tab === 'materials') {
      next.delete('tab')
    } else {
      next.set('tab', tab)
    }
    setSearchParams(next)
  }

  const [partners, setPartners] = useState<Partner[] | null>(null)
  const [partnersError, setPartnersError] = useState<string | null>(null)
  const [missingPartnerNotice, setMissingPartnerNotice] = useState<string | null>(null)
  const [sidebarOpen, setSidebarOpen] = useState(false)
  const [createOpen, setCreateOpen] = useState(false)
  const [editOpen, setEditOpen] = useState(false)
  const [logoutError, setLogoutError] = useState<string | null>(null)

  async function handleLogout() {
    setLogoutError(null)
    try {
      await logout()
    } catch (err) {
      setLogoutError(err instanceof Error ? err.message : 'Не удалось выйти. Попробуйте ещё раз.')
    }
  }

  const loadPartners = useCallback(() => {
    setPartnersError(null)
    // runRead, not a bare listPartners(): a 401 here means the session is
    // gone, and must raise the re-login prompt rather than a retry banner.
    return runRead(() => listPartners()).then(
      (items) => setPartners(items),
      (err: unknown) => setPartnersError(err instanceof Error ? err.message : 'Не удалось загрузить партнёров.'),
    )
  }, [runRead])

  useEffect(() => {
    void loadPartners()
  }, [loadPartners])

  const selectedPartner = partners?.find((p) => p.id === partnerId) ?? null

  // Restore the selected partner after a reload even if it is not (yet) in
  // the loaded list — e.g. a direct link, or created in another tab.
  useEffect(() => {
    if (!partnerId || !partners || selectedPartner) {
      if (partnerId && partners && selectedPartner) setMissingPartnerNotice(null)
      return
    }
    let cancelled = false
    runRead(() => getPartner(partnerId)).then(
      (partner) => {
        if (cancelled) return
        setPartners((prev) => (prev ? [...prev, partner] : [partner]))
      },
      (err: unknown) => {
        if (cancelled) return
        // A 404 is still handled here; runRead only intercepts 401 and
        // rethrows everything else untouched.
        if (err instanceof ApiError && err.status === 404) {
          setMissingPartnerNotice('Партнёр не найден. Возможно, ссылка устарела.')
          const next = new URLSearchParams(searchParams)
          next.delete('partner_id')
          setSearchParams(next, { replace: true })
        } else {
          setMissingPartnerNotice(err instanceof Error ? err.message : 'Не удалось загрузить партнёра.')
        }
      },
    )
    return () => {
      cancelled = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [partnerId, partners, selectedPartner])

  function selectPartner(partner: Partner) {
    const next = new URLSearchParams(searchParams)
    next.set('partner_id', partner.id)
    setSearchParams(next)
    setSidebarOpen(false)
    setMissingPartnerNotice(null)
  }

  async function handleCreate(input: { name: string; note?: string }) {
    const partner = await runMutation((token) => createPartner(input, token))
    setPartners((prev) => (prev ? [...prev, partner] : [partner]))
    setCreateOpen(false)
    selectPartner(partner)
  }

  async function handleEdit(input: { name: string; note?: string }) {
    if (!selectedPartner) return
    const updated = await runMutation((token) => updatePartner(selectedPartner.id, input, token))
    setPartners((prev) => (prev ? prev.map((p) => (p.id === updated.id ? updated : p)) : prev))
    setEditOpen(false)
  }

  return (
    <div>
      <div className="mobile-topbar">
        <button
          type="button"
          className="mobile-menu-btn"
          aria-expanded={sidebarOpen}
          aria-controls="sidebar"
          onClick={() => setSidebarOpen((v) => !v)}
        >
          <svg width="18" height="14" viewBox="0 0 18 14" fill="none" aria-hidden="true">
            <path d="M0 1h18M0 7h18M0 13h18" stroke="currentColor" strokeWidth="1.6" />
          </svg>
          <span className="visually-hidden">Партнёры</span>
        </button>
        <strong>OTDEL</strong>
      </div>

      <div className="shell">
        {partnersError ? (
          <div style={{ padding: 16 }}>
            <StatusMessage tone="error" onRetry={loadPartners}>
              {partnersError}
            </StatusMessage>
          </div>
        ) : (
          <PartnerSidebar
            partners={partners ?? []}
            selectedId={selectedPartner?.id ?? null}
            onSelect={selectPartner}
            onAdd={() => setCreateOpen(true)}
            isOpen={sidebarOpen}
          />
        )}

        <main className="workspace">
          <div className="workspace-header">
            <span aria-hidden="true" />
            <button type="button" className="button button-ghost button-small" onClick={() => void handleLogout()}>
              Выйти
            </button>
          </div>

          {logoutError ? (
            <StatusMessage tone="error" onRetry={() => void handleLogout()}>
              {logoutError}
            </StatusMessage>
          ) : null}

          {missingPartnerNotice ? <StatusMessage tone="warn">{missingPartnerNotice}</StatusMessage> : null}

          {partners === null && !partnersError ? <StatusMessage>Загружаем партнёров…</StatusMessage> : null}

          {partners && partners.length === 0 && !partnerId ? (
            <div className="empty-state">
              <img src="/otto/welcome.png" alt="" aria-hidden="true" width="120" height="120" />
              <h2>Партнёров пока нет</h2>
              <p>Добавьте первого партнёра, чтобы начать загружать материалы.</p>
            </div>
          ) : null}

          {partners && partners.length > 0 && !selectedPartner && !partnerId ? (
            <div className="empty-state">
              <img src="/otto/question.png" alt="" aria-hidden="true" width="120" height="120" />
              <h2>Выберите партнёра</h2>
              <p>Список партнёров — слева. Выберите одного, чтобы увидеть его материалы.</p>
            </div>
          ) : null}

          {selectedPartner ? (
            <>
              <header className="partner-header">
                <div className="partner-header__info">
                  <h1>{selectedPartner.name}</h1>
                </div>
                <div className="partner-header__actions">
                  <button type="button" className="button button-outline button-small" onClick={() => setEditOpen(true)}>
                    Изменить
                  </button>
                </div>
              </header>

              <nav className="tabs" aria-label="Разделы карточки партнёра">
                {TABS.map((tab) => (
                  <button
                    key={tab.id}
                    type="button"
                    className="tab"
                    aria-current={activeTab === tab.id ? 'page' : undefined}
                    onClick={() => selectTab(tab.id)}
                  >
                    {tab.label}
                  </button>
                ))}
              </nav>

              {/* key=partner id: forces a full remount (fresh polling hook,
                  fresh retry/upload-dialog state) on every partner switch,
                  so no in-flight request/timer from the previous partner can
                  ever land on the new one. */}
              {activeTab === 'materials' ? (
                <MaterialsPanel key={selectedPartner.id} partnerId={selectedPartner.id} partnerName={selectedPartner.name} />
              ) : activeTab === 'knowledge' ? (
                <KnowledgePanel key={selectedPartner.id} partnerId={selectedPartner.id} />
              ) : (
                <ResearchPanel key={selectedPartner.id} partnerId={selectedPartner.id} />
              )}
            </>
          ) : null}
        </main>
      </div>

      <PartnerFormDialog open={createOpen} mode="create" onClose={() => setCreateOpen(false)} onSubmit={handleCreate} />
      {selectedPartner ? (
        <PartnerFormDialog
          open={editOpen}
          mode="edit"
          partner={selectedPartner}
          onClose={() => setEditOpen(false)}
          onSubmit={handleEdit}
        />
      ) : null}
    </div>
  )
}
