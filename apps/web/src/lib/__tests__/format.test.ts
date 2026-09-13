import { describe, expect, it } from 'vitest'
import { formatBytes, materialStatusPresentation } from '../format'

describe('formatBytes', () => {
  it('renders bytes below 1024 as Б', () => {
    expect(formatBytes(512)).toBe('512 Б')
  })

  it('renders KiB with one decimal below 10', () => {
    expect(formatBytes(1536)).toBe('1.5 КиБ')
  })

  it('renders whole MiB without decimals at 10 or above', () => {
    expect(formatBytes(25 * 1024 * 1024)).toBe('25 МиБ')
  })
})

describe('materialStatusPresentation', () => {
  it('labels "queued" as waiting to be processed, not as already read', () => {
    const presentation = materialStatusPresentation('queued')
    expect(presentation.label).toBe('В очереди')
    expect(presentation.defaultHint).toMatch(/ожидает обработки/i)
  })

  it('gives "partial" a warn tone, distinct from full success', () => {
    expect(materialStatusPresentation('partial').tone).toBe('warn')
    expect(materialStatusPresentation('completed').tone).toBe('success')
  })
})
