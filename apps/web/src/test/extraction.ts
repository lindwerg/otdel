import type {
  AmbiguityReason,
  BoundingBox,
  CellRole,
  CellUsability,
  CellValueKind,
  ConditionRef,
  ContextRef,
  PageRegion,
  SourceSpan,
  TableCell,
  UnitRef,
} from '../api/types'

/**
 * Builders for R03 source-evidence shapes, shared by the page tests.
 *
 * These live here rather than in each test file because the shapes are wide: a
 * cell now carries a role, a verdict, its structural context and its span, and
 * three test files repeating all of that would drift apart exactly where the
 * honesty rules live. Nothing here is used by application code.
 */

export function exactSpan(pageNumber: number, bbox: BoundingBox): SourceSpan {
  return { page_number: pageNumber, bbox, state: 'exact' }
}

export function unlocatedSpan(pageNumber: number, reason: string): SourceSpan {
  return { page_number: pageNumber, bbox: null, state: 'unavailable', reason }
}

export interface CellOptions {
  row: number
  column: number
  raw: string
  regionId?: string
  valueKind?: CellValueKind
  role?: CellRole
  usability?: CellUsability
  reasons?: AmbiguityReason[]
  columnHeaderPath?: ContextRef[]
  rowHeaderPath?: ContextRef[]
  subject?: ContextRef | null
  property?: ContextRef | null
  unit?: UnitRef | null
  conditions?: ConditionRef[]
  span?: SourceSpan
  pageNumber?: number
}

export function cell(options: CellOptions): TableCell {
  const role = options.role ?? 'data'
  const regionId = options.regionId ?? 'region-table'
  const pageNumber = options.pageNumber ?? 3
  return {
    id: `cell-${options.row}-${options.column}`,
    region_id: regionId,
    row_index: options.row,
    column_index: options.column,
    is_header: role !== 'data',
    raw_text: options.raw,
    value_kind: options.valueKind ?? (options.raw === '' ? 'empty' : 'text'),
    unit: options.unit?.unit ?? null,
    column_header: options.property?.text ?? null,
    bbox: null,
    role,
    verdict: {
      usability: options.usability ?? 'usable',
      reasons: options.reasons ?? [],
    },
    structural_context: {
      column_header_path: options.columnHeaderPath ?? [],
      row_header_path: options.rowHeaderPath ?? [],
      subject: options.subject ?? null,
      property: options.property ?? null,
      unit: options.unit ?? null,
      conditions: options.conditions ?? [],
    },
    span: options.span ?? unlocatedSpan(pageNumber, 'координаты ячейки не сохранены'),
  }
}

/**
 * The table that produced the reported defect.
 *
 * Its third column is headed «безопасная рабочая нагрузка (Н)» — a label that
 * was once published as if it were a measurement — and the fixture keeps that
 * exact wording so the tests assert the real case rather than a sanitised one.
 */
export function loadTableRegion(overrides: Partial<PageRegion> = {}): PageRegion {
  const bbox = { x0: 50, y0: 620, x1: 460, y1: 710 }
  return {
    id: 'region-table',
    page_id: 'page-3',
    page_number: 3,
    ordinal: 0,
    kind: 'table',
    text: 'Профиль Длина, мм безопасная рабочая нагрузка (Н)',
    source: 'text_layer',
    bbox,
    row_count: 3,
    column_count: 3,
    span: exactSpan(3, bbox),
    cells: [
      cell({
        row: 0,
        column: 0,
        raw: 'Профиль',
        role: 'column_header',
        usability: 'unusable',
        reasons: ['header_is_not_a_value'],
      }),
      cell({
        row: 0,
        column: 1,
        raw: 'Длина, мм',
        role: 'column_header',
        usability: 'unusable',
        reasons: ['header_is_not_a_value'],
      }),
      cell({
        row: 0,
        column: 2,
        raw: 'безопасная рабочая нагрузка (Н)',
        role: 'column_header',
        usability: 'unusable',
        reasons: ['header_is_not_a_value'],
      }),

      cell({
        row: 1,
        column: 0,
        raw: 'BP21',
        role: 'row_header',
        usability: 'unusable',
        reasons: ['header_is_not_a_value'],
      }),
      cell({
        row: 1,
        column: 1,
        raw: '1200',
        valueKind: 'number',
        subject: { text: 'BP21', origin: 'row_label' },
        property: { text: 'Длина', origin: 'header_row' },
        unit: { unit: 'мм', origin: 'header_row' },
      }),
      // Fully attributed: product, property, value, unit and the footnote that
      // says under which support scheme the load applies.
      cell({
        row: 1,
        column: 2,
        raw: '4860',
        valueKind: 'number',
        subject: { text: 'BP21', origin: 'row_label' },
        property: { text: 'безопасная рабочая нагрузка', origin: 'header_row' },
        unit: { unit: 'Н', origin: 'header_row' },
        conditions: [
          {
            text: 'при опирании на две опоры',
            marker: '*',
            span: exactSpan(3, { x0: 50, y0: 540, x1: 300, y1: 556 }),
          },
        ],
      }),

      cell({
        row: 2,
        column: 0,
        raw: 'BP40',
        role: 'row_header',
        usability: 'unusable',
        reasons: ['header_is_not_a_value'],
      }),
      // The row label was blank and carried down across a merged cell.
      cell({
        row: 2,
        column: 1,
        raw: '2000',
        valueKind: 'number',
        usability: 'ambiguous',
        reasons: ['context_inherited_from_merged_cell'],
        subject: { text: 'BP40', origin: 'inherited_from_merged_row_label' },
        property: { text: 'Длина', origin: 'header_row' },
        unit: { unit: 'мм', origin: 'header_row' },
      }),
      // The source does not print this load. Blank stays blank.
      cell({
        row: 2,
        column: 2,
        raw: '',
        valueKind: 'empty',
        usability: 'unusable',
        reasons: ['blank_cell'],
        subject: { text: 'BP40', origin: 'inherited_from_merged_row_label' },
        property: { text: 'безопасная рабочая нагрузка', origin: 'header_row' },
      }),
    ],
    ...overrides,
  }
}
