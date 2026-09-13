import type { Partner } from '../api/types'

interface PartnerSidebarProps {
  partners: Partner[]
  selectedId: string | null
  onSelect: (partner: Partner) => void
  onAdd: () => void
  isOpen: boolean
}

export function PartnerSidebar({ partners, selectedId, onSelect, onAdd, isOpen }: PartnerSidebarProps) {
  return (
    <aside className={`sidebar${isOpen ? ' is-open' : ''}`} id="sidebar">
      <div className="login-card__brand">
        <span className="brand-mark" aria-hidden="true">
          <i></i>
          <i></i>
          <i></i>
        </span>
        OTDEL
      </div>
      <nav aria-label="Партнёры">
        <span className="nav-label">Партнёры</span>
        {partners.length === 0 ? (
          <p style={{ color: 'var(--muted)', fontSize: '0.8125rem' }}>Партнёров пока нет.</p>
        ) : (
          <ul className="partner-list">
            {partners.map((partner) => (
              <li key={partner.id}>
                <button
                  type="button"
                  className="partner-item"
                  aria-current={partner.id === selectedId ? 'true' : undefined}
                  onClick={() => onSelect(partner)}
                >
                  <strong>{partner.name}</strong>
                </button>
              </li>
            ))}
          </ul>
        )}
      </nav>
      <button type="button" className="button button-primary sidebar-cta" onClick={onAdd}>
        + Добавить партнёра
      </button>
    </aside>
  )
}
