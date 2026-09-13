import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import type { Partner } from '../../api/types'
import { PartnerFormDialog } from '../PartnerFormDialog'

const partner: Partner = {
  id: 'p1',
  name: 'BASIS',
  note: 'старое пояснение',
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

describe('PartnerFormDialog', () => {
  it('rejects an empty name with an inline, accessible error instead of calling onSubmit', async () => {
    const user = userEvent.setup()
    const onSubmit = vi.fn()
    render(<PartnerFormDialog open mode="create" onClose={vi.fn()} onSubmit={onSubmit} />)

    await user.click(screen.getByRole('button', { name: 'Создать' }))

    expect(onSubmit).not.toHaveBeenCalled()
    expect(screen.getByText('Введите название партнёра.')).toBeInTheDocument()
    expect(screen.getByLabelText(/Название/)).toHaveAttribute('aria-invalid', 'true')
  })

  it('omits `note` on create when left blank (nothing to clear yet)', async () => {
    const user = userEvent.setup()
    const onSubmit = vi.fn().mockResolvedValue(undefined)
    render(<PartnerFormDialog open mode="create" onClose={vi.fn()} onSubmit={onSubmit} />)

    await user.type(screen.getByLabelText(/Название/), 'Новый партнёр')
    await user.click(screen.getByRole('button', { name: 'Создать' }))

    expect(onSubmit).toHaveBeenCalledWith({ name: 'Новый партнёр' })
  })

  it('sends an explicit empty `note` when editing clears a previously-set note', async () => {
    const user = userEvent.setup()
    const onSubmit = vi.fn().mockResolvedValue(undefined)
    render(<PartnerFormDialog open mode="edit" partner={partner} onClose={vi.fn()} onSubmit={onSubmit} />)

    const noteField = screen.getByLabelText(/Пояснение/) as HTMLTextAreaElement
    expect(noteField.value).toBe('старое пояснение')
    await user.clear(noteField)
    await user.click(screen.getByRole('button', { name: 'Сохранить' }))

    // Must be an explicit '', not omitted — omitting would leave the old
    // note unchanged server-side per the PATCH contract.
    expect(onSubmit).toHaveBeenCalledWith({ name: 'BASIS', note: '' })
  })
})
