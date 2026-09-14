import { useState } from 'react'
import type { KnowledgeFact, ProductNode } from '../../api/types'
import { factKindLabel, factValueLine } from '../../lib/format'
import { EvidenceList } from './EvidenceList'

interface ProductFactsProps {
  partnerId: string
  nodes: ProductNode[]
}

/**
 * One fact: what it says, under which conditions, and — when opened — the
 * fragment of the document it came from.
 *
 * The value is printed exactly as recorded, with the unit appended only if the
 * server stored one (it stores a unit only when the source writes it). The
 * conditions are shown next to the value rather than hidden behind the
 * disclosure, because a load without its support scheme is not a smaller truth
 * — it is a different one.
 */
function FactItem({ partnerId, fact }: { partnerId: string; fact: KnowledgeFact }) {
  const [open, setOpen] = useState(false)

  return (
    <li className="fact" data-kind={fact.kind}>
      <div className="fact__head">
        <span className="fact__attribute">{fact.attribute}</span>
        <span className="fact__value">{factValueLine(fact)}</span>
        <span className="fact__kind">{factKindLabel(fact.kind)}</span>
      </div>

      {fact.conditions ? (
        <p className="fact__conditions">
          <span className="fact__label">условия:</span> {fact.conditions}
        </p>
      ) : null}

      <div className="fact__actions">
        <button
          type="button"
          className="button button-ghost button-small"
          aria-expanded={open}
          onClick={() => setOpen((value) => !value)}
        >
          {open ? 'Скрыть источник' : 'Показать источник'}
        </button>
        <span className="fact__status">
          {/* Явно: 1C только предлагает. Проверка и публикация — следующий этап. */}
          черновик, источником подтверждена цитата — не независимая проверка
        </span>
      </div>

      {open ? (
        <EvidenceList
          partnerId={partnerId}
          evidence={fact.evidence}
          modelContext={fact.model_context}
        />
      ) : null}
    </li>
  )
}

export function ProductFacts({ partnerId, nodes }: ProductFactsProps) {
  if (nodes.length === 0) {
    return (
      <p className="knowledge-note">
        Продуктов пока нет. Они появятся после разбора прочитанного материала.
      </p>
    )
  }

  return (
    <ul className="product-list" aria-label="Продукты и характеристики">
      {nodes.map((node) => (
        <li key={node.product?.id ?? 'partner-level'} className="product">
          <div className="product__head">
            <h4 className="product__name">
              {node.product ? node.product.name : 'Общие сведения о предложении'}
            </h4>
            {node.category ? (
              <span className="product__category">{node.category.name}</span>
            ) : null}
            {node.product?.kind === 'service' ? (
              <span className="product__kind">услуга</span>
            ) : null}
          </div>

          {node.product?.summary ? (
            <p className="product__summary">{node.product.summary}</p>
          ) : null}

          {node.facts.length === 0 ? (
            <p className="knowledge-note">
              Характеристик с подтверждённой цитатой для этого продукта нет.
            </p>
          ) : (
            <ul className="fact-list">
              {node.facts.map((fact) => (
                <FactItem key={fact.id} partnerId={partnerId} fact={fact} />
              ))}
            </ul>
          )}
        </li>
      ))}
    </ul>
  )
}
