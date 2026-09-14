import type {
  AliasRelation,
  ApplicationDetailKind,
  CoverageReport,
  CoverageState,
  DeclarationTopic,
  GapNature,
  IdentityBasis,
  IdentityState,
  PageDisposition,
  RequirementsState,
  StructuralSource,
  SynonymRelation,
  UncertaintyKind,
} from '../api/types'

/**
 * The reader-facing wording of the R05 vocabulary.
 *
 * Its own module rather than an addition to `format.ts`: that file is already at
 * the size where a reader stops finding things in it, and this vocabulary is one
 * coherent subject.
 *
 * Every label below follows one rule. **A state that means "nobody checked"
 * never reads like a state that means "checked and fine".** «не проверено» and
 * «разобрано полностью» must not be mistakable for each other at a glance,
 * because the audited run was reported as the second while being the first.
 */

const COVERAGE_STATE_LABEL: Record<CoverageState, string> = {
  unknown: 'охват не оценивался',
  complete: 'разобраны все страницы',
  partial_accounted: 'разобрано не всё, причины указаны',
  incomplete: 'разбор не завершён',
}

export function coverageStateLabel(state: CoverageState): string {
  return COVERAGE_STATE_LABEL[state] ?? state
}

/**
 * The tone a coverage state should be rendered in.
 *
 * `unknown` is deliberately not neutral: an unjudged run is a problem, and
 * rendering it in the same grey as "all good" is how it stops being noticed.
 */
export function coverageTone(state: CoverageState): 'good' | 'warn' | 'bad' {
  if (state === 'complete') return 'good'
  if (state === 'partial_accounted') return 'warn'
  return 'bad'
}

const REQUIREMENTS_LABEL: Record<RequirementsState, string> = {
  unknown: 'состав паспорта не проверялся',
  met: 'паспорт собран полностью',
  unmet: 'паспорту не хватает данных',
}

export function requirementsLabel(state: RequirementsState): string {
  return REQUIREMENTS_LABEL[state] ?? state
}

export function requirementsTone(state: RequirementsState): 'good' | 'warn' | 'bad' {
  if (state === 'met') return 'good'
  if (state === 'unmet') return 'warn'
  return 'bad'
}

const DISPOSITION_LABEL: Record<PageDisposition, string> = {
  processed: 'разобрана',
  deferred_budget: 'отложена: исчерпан бюджет запросов',
  unreadable_needs_ocr: 'ждёт распознавания: текстового слоя нет',
  unreadable_failed: 'не удалось прочитать',
  unreadable_empty: 'пустая',
  not_read_yet: 'ещё не прочитана',
  not_offered_no_text: 'прочитана, текста не оказалось',
  excluded_by_request: 'не входила в этот проход',
}

export function dispositionLabel(disposition: PageDisposition): string {
  return DISPOSITION_LABEL[disposition] ?? disposition
}

const GAP_NATURE_LABEL: Record<GapNature, string> = {
  commercial: 'коммерческий',
  technical: 'технический',
  other: 'прочий',
}

export function gapNatureLabel(nature: GapNature): string {
  return GAP_NATURE_LABEL[nature] ?? nature
}

const ALIAS_RELATION_LABEL: Record<AliasRelation, string> = {
  alias: 'то же изделие',
  sense: 'более узкое значение',
  // Never «возможно, то же»: an interface that hedges towards identity is an
  // interface that will eventually be read as asserting one.
  unclear: 'связь не подтверждена материалом',
}

export function aliasRelationLabel(relation: AliasRelation): string {
  return ALIAS_RELATION_LABEL[relation] ?? relation
}

const SYNONYM_RELATION_LABEL: Record<SynonymRelation, string> = {
  synonym: 'то же понятие',
  abbreviation: 'сокращение',
  unclear: 'связь не подтверждена материалом',
}

export function synonymRelationLabel(relation: SynonymRelation): string {
  return SYNONYM_RELATION_LABEL[relation] ?? relation
}

/** Whether a recorded surface form may be followed as if the two names were one. */
export function isSafeToFollow(relation: AliasRelation | SynonymRelation): boolean {
  return relation === 'alias' || relation === 'synonym' || relation === 'abbreviation'
}

const IDENTITY_STATE_LABEL: Record<IdentityState, string> = {
  linked: 'предложено считать одним изделием',
  unclear: 'совпадение не подтверждено',
}

export function identityStateLabel(state: IdentityState): string {
  return IDENTITY_STATE_LABEL[state] ?? state
}

const IDENTITY_BASIS_LABEL: Record<IdentityBasis, string> = {
  identical_designation_quoted: 'одно и то же обозначение процитировано с обеих сторон',
  alias_quoted_in_both: 'записанное написание встречается в обоих материалах',
  name_similarity_only: 'совпадают только названия — это не доказательство',
}

export function identityBasisLabel(basis: IdentityBasis): string {
  return IDENTITY_BASIS_LABEL[basis] ?? basis
}

const DETAIL_KIND_LABEL: Record<ApplicationDetailKind, string> = {
  parameter: 'что нужно знать',
  constraint: 'что ограничивает',
  question: 'что надо спросить',
}

export function detailKindLabel(kind: ApplicationDetailKind): string {
  return DETAIL_KIND_LABEL[kind] ?? kind
}

const UNCERTAINTY_KIND_LABEL: Record<UncertaintyKind, string> = {
  ambiguous_table_cell: 'ячейку таблицы нельзя прочитать однозначно',
  unreadable_page: 'страница не прочитана — что на ней, неизвестно',
  unresolved_unit: 'единица измерения нигде не написана',
  unresolved_subject: 'непонятно, к какому изделию относится значение',
  uninterpreted_diagram: 'схема нагрузок распознана, но не истолкована',
}

export function uncertaintyKindLabel(kind: UncertaintyKind): string {
  return UNCERTAINTY_KIND_LABEL[kind] ?? kind
}

const DECLARATION_TOPIC_LABEL: Record<DeclarationTopic, string> = {
  glossary: 'термины',
  questions: 'вопросы',
  applications: 'задачи применения',
  commercial_unknowns: 'коммерческие неизвестные',
  technical_unknowns: 'технические неизвестные',
}

export function declarationTopicLabel(topic: DeclarationTopic): string {
  return DECLARATION_TOPIC_LABEL[topic] ?? topic
}

const STRUCTURAL_SOURCE_LABEL: Record<StructuralSource, string> = {
  page_text: 'из текста страницы',
  table_cell: 'из ячейки таблицы',
}

export function structuralSourceLabel(source: StructuralSource): string {
  return STRUCTURAL_SOURCE_LABEL[source] ?? source
}

/**
 * The page account in one line: «разобрано 36 из 44».
 *
 * The denominator is never omitted, even when it equals the numerator. «36
 * страниц» is the sentence the audit could not act on; «36 из 44» is the one it
 * needed.
 */
export function coverageLine(report: CoverageReport): string {
  const parts = [`разобрано ${report.pages_processed} из ${report.pages_total}`]
  if (report.pages_deferred > 0) parts.push(`отложено ${report.pages_deferred}`)
  if (report.pages_unreadable > 0) parts.push(`не прочитано ${report.pages_unreadable}`)
  return parts.join(', ')
}

/**
 * Split a `name: explanation` line from `requirements_missing`.
 *
 * The server writes both halves; showing only the name would be jargon and
 * showing only the explanation would lose the handle the two layers share.
 */
export function splitRequirement(line: string): { name: string; explanation: string } {
  const at = line.indexOf(':')
  if (at < 0) return { name: '', explanation: line }
  return { name: line.slice(0, at).trim(), explanation: line.slice(at + 1).trim() }
}

/** Cost in whole cents, or null when the provider reported none. Never "free". */
export function costLine(microUsd: number | null): string | null {
  if (microUsd == null) return null
  return `${(microUsd / 1_000_000).toFixed(4)} $`
}
