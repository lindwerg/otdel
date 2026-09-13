import { describe, expect, it } from 'vitest'
import { MAX_UPLOAD_BYTES } from '../../api/client'
import { validateFileSize, validatePartnerName, validatePartnerNote } from '../validation'

describe('validatePartnerName', () => {
  it('rejects an empty/whitespace-only name', () => {
    const result = validatePartnerName('   ')
    expect(result.ok).toBe(false)
  })

  it('trims surrounding whitespace on success', () => {
    const result = validatePartnerName('  BASIS  ')
    expect(result).toEqual({ ok: true, value: 'BASIS' })
  })

  it('rejects a name over 200 characters (after trim)', () => {
    const result = validatePartnerName('a'.repeat(201))
    expect(result.ok).toBe(false)
  })

  it('accepts exactly 200 characters', () => {
    const result = validatePartnerName('a'.repeat(200))
    expect(result.ok).toBe(true)
  })
})

describe('validatePartnerNote', () => {
  it('accepts an empty note (optional field)', () => {
    expect(validatePartnerNote('')).toEqual({ ok: true, value: '' })
  })

  it('rejects a note over 10000 characters', () => {
    const result = validatePartnerNote('a'.repeat(10001))
    expect(result.ok).toBe(false)
  })
})

function makeFile(sizeBytes: number, name = 'file.pdf'): File {
  return new File([new Uint8Array(sizeBytes)], name, { type: 'application/pdf' })
}

describe('validateFileSize', () => {
  it('accepts a file exactly at the 25 MiB limit', () => {
    expect(validateFileSize(makeFile(MAX_UPLOAD_BYTES))).toBeNull()
  })

  it('rejects a file one byte over the limit, naming the file', () => {
    const message = validateFileSize(makeFile(MAX_UPLOAD_BYTES + 1, 'big.pdf'))
    expect(message).toContain('big.pdf')
    expect(message).toMatch(/25/)
  })

  it('rejects an empty file', () => {
    expect(validateFileSize(makeFile(0))).toMatch(/пустой/)
  })
})
