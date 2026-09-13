import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { describe, expect, it } from 'vitest'
import { Dialog } from '../Dialog'

function Harness({ preventClose }: { preventClose?: boolean }) {
  const [open, setOpen] = useState(false)
  return (
    <div>
      <button type="button" onClick={() => setOpen(true)}>
        Открыть
      </button>
      <Dialog open={open} onClose={() => setOpen(false)} labelledBy="dlg-title" preventClose={preventClose}>
        <h2 id="dlg-title">Заголовок</h2>
        <button type="button" onClick={() => setOpen(false)}>
          Закрыть
        </button>
      </Dialog>
    </div>
  )
}

describe('Dialog', () => {
  it('opens via showModal and returns focus to the exact trigger button on Escape', async () => {
    const user = userEvent.setup()
    render(<Harness />)

    const trigger = screen.getByRole('button', { name: 'Открыть' })
    await user.click(trigger)
    expect(screen.getByText('Заголовок')).toBeVisible()

    await user.keyboard('{Escape}')

    expect(screen.queryByText('Заголовок')).not.toBeInTheDocument()
    expect(trigger).toHaveFocus()
  })

  it('blocks Escape while preventClose is true, so a busy dialog cannot be closed out from under itself', async () => {
    const user = userEvent.setup()
    render(<Harness preventClose />)

    await user.click(screen.getByRole('button', { name: 'Открыть' }))
    expect(screen.getByText('Заголовок')).toBeVisible()

    await user.keyboard('{Escape}')

    // Still open: the dialog must not disappear while a submit/upload is in flight.
    expect(screen.getByText('Заголовок')).toBeVisible()
  })

  it('closes and refocuses when the explicit close button is used', async () => {
    const user = userEvent.setup()
    render(<Harness />)

    const trigger = screen.getByRole('button', { name: 'Открыть' })
    await user.click(trigger)
    await user.click(screen.getByRole('button', { name: 'Закрыть' }))

    expect(screen.queryByText('Заголовок')).not.toBeInTheDocument()
    expect(trigger).toHaveFocus()
  })
})
