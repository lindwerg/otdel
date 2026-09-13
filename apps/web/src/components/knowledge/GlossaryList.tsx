import { useState } from 'react'
import type { GlossaryTerm, QaEntry } from '../../api/types'
import { EvidenceList } from './EvidenceList'

interface GlossaryListProps {
  partnerId: string
  terms: GlossaryTerm[]
}

/**
 * The glossary.
 *
 * A definition copied from the document and a definition the model wrote are
 * shown differently, because they are different things: the second is a
 * paraphrase nobody has checked. Both carry the fragment the term was read
 * from, so an ambiguous term can be judged against its context rather than
 * against the wording of the definition.
 */
export function GlossaryList({ partnerId, terms }: GlossaryListProps) {
  if (terms.length === 0) {
    return <p className="knowledge-note">Терминов пока нет.</p>
  }

  return (
    <ul className="term-list" aria-label="Глоссарий">
      {terms.map((term) => (
        <li key={term.id} className="term">
          <div className="term__head">
            <span className="term__name">{term.term}</span>
            {term.definition_is_model_context ? (
              <span className="term__badge">формулировка модели</span>
            ) : (
              <span className="term__badge term__badge--source">из источника</span>
            )}
          </div>
          <p className="term__definition">{term.definition}</p>
          <EvidenceList partnerId={partnerId} evidence={term.evidence} />
        </li>
      ))}
    </ul>
  )
}

interface QaListProps {
  partnerId: string
  entries: QaEntry[]
}

/** Questions answered from this partner's own materials, each with its source. */
export function QaList({ partnerId, entries }: QaListProps) {
  const [openId, setOpenId] = useState<string | null>(null)

  if (entries.length === 0) {
    return <p className="knowledge-note">Вопросов и ответов по материалам пока нет.</p>
  }

  return (
    <ul className="qa-list" aria-label="Вопросы и ответы по материалам">
      {entries.map((entry) => (
        <li key={entry.id} className="qa">
          <p className="qa__question">{entry.question}</p>
          <p className="qa__answer">{entry.answer}</p>
          {/* An answer is a sentence the model composed. Saying so keeps the
              quotation below it from appearing to vouch for the wording above
              it — the same distinction the glossary makes for a definition. */}
          {entry.answer_is_model_context ? (
            <span className="qa__badge">ответ сформулирован моделью — не цитата</span>
          ) : (
            <span className="qa__badge qa__badge--source">ответ дословно из источника</span>
          )}
          <button
            type="button"
            className="button button-ghost button-small"
            aria-expanded={openId === entry.id}
            onClick={() => setOpenId((current) => (current === entry.id ? null : entry.id))}
          >
            {openId === entry.id ? 'Скрыть источник' : 'Показать источник'}
          </button>
          {openId === entry.id ? (
            <EvidenceList partnerId={partnerId} evidence={entry.evidence} />
          ) : null}
        </li>
      ))}
    </ul>
  )
}
