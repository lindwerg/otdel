import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter } from 'react-router-dom'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { App } from '../App'
import { AuthProvider } from '../auth/AuthContext'
import { apiError, installMockFetch, jsonResponse, type RecordedRequest } from '../test/mockFetch'

/**
 * End-to-end (component-level) coverage of the two ways a session can end,
 * which the UI must treat completely differently:
 *
 *  - it *expires* under the user's hands → their half-filled form survives
 *    and they re-authenticate on top of it;
 *  - they *log out* on purpose → the workspace and everything drafted in it
 *    is torn down, and they get the ordinary login screen.
 */

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

interface Scenario {
  /** Answers POST /api/partners; default is a plain success. */
  createPartner?: (callCount: number) => Response
}

function renderApp(scenario: Scenario = {}) {
  let createCalls = 0
  const { calls } = installMockFetch((req: RecordedRequest) => {
    if (req.url === '/api/session' && req.method === 'GET') {
      return jsonResponse(200, { authenticated: true, csrf_token: 'csrf-1' })
    }
    if (req.url === '/api/session' && req.method === 'POST') {
      return jsonResponse(200, { authenticated: true, csrf_token: 'csrf-2' })
    }
    if (req.url === '/api/session' && req.method === 'DELETE') {
      return new Response(null, { status: 204 })
    }
    if (req.url === '/api/partners' && req.method === 'GET') {
      return jsonResponse(200, { items: [] })
    }
    if (req.url === '/api/partners' && req.method === 'POST') {
      createCalls += 1
      if (scenario.createPartner) return scenario.createPartner(createCalls)
      return jsonResponse(201, {
        id: 'p1',
        name: 'BASIS',
        note: null,
        created_at: '2026-01-01T00:00:00Z',
        updated_at: '2026-01-01T00:00:00Z',
      })
    }
    if (/^\/api\/partners\/[^/]+\/materials$/.test(req.url) && req.method === 'GET') {
      return jsonResponse(200, { items: [] })
    }
    throw new Error(`unhandled request in test: ${req.method} ${req.url}`)
  })

  const view = render(
    <MemoryRouter>
      <AuthProvider>
        <App />
      </AuthProvider>
    </MemoryRouter>,
  )
  return { ...view, calls }
}

/** The create-partner dialog, identified by its own heading. */
function partnerDialog(): HTMLDialogElement {
  const heading = screen.getByRole('heading', { name: 'Добавить партнёра' })
  const dialog = heading.closest('dialog')
  if (!dialog) throw new Error('partner form is not inside a native <dialog>')
  return dialog as HTMLDialogElement
}

async function openPartnerDraft(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole('button', { name: '+ Добавить партнёра' }))
  await user.type(screen.getByLabelText(/Название/), 'BASIS')
  await user.type(screen.getByLabelText(/Пояснение/), 'черновик, который нельзя потерять')
}

describe('session expiry while a modal dialog is open', () => {
  it('prompts for the password in a dialog stacked on top, keeps the draft, and resumes the same work', async () => {
    const user = userEvent.setup()
    const showModal = vi.spyOn(HTMLDialogElement.prototype, 'showModal')
    const { calls } = renderApp({
      // First submit: the session died. Second (after re-login): success.
      createPartner: (n) =>
        n === 1
          ? apiError(401, 'unauthorized', 'Сессия недействительна')
          : jsonResponse(201, {
              id: 'p1',
              name: 'BASIS',
              note: 'черновик, который нельзя потерять',
              created_at: '2026-01-01T00:00:00Z',
              updated_at: '2026-01-01T00:00:00Z',
            }),
    })

    await openPartnerDraft(user)
    const formDialog = partnerDialog()
    expect(formDialog).toHaveAttribute('open')

    await user.click(screen.getByRole('button', { name: 'Создать' }))

    // --- The re-login prompt itself -------------------------------------
    const prompt = await screen.findByRole('alertdialog')
    // It must be a *native modal* dialog: an ordinary positioned <div>, no
    // matter its z-index, is painted below an already-open modal <dialog>
    // and is inert there — the user could never reach it.
    expect(prompt.tagName).toBe('DIALOG')
    expect(prompt).toHaveAttribute('open')
    // ...and it must have entered the top layer *after* the partner dialog,
    // i.e. above it, not below.
    expect(showModal.mock.contexts.at(-1)).toBe(prompt)
    expect(showModal.mock.contexts).toContain(formDialog)
    expect(showModal.mock.contexts.indexOf(prompt)).toBeGreaterThan(
      showModal.mock.contexts.indexOf(formDialog),
    )

    const password = within(prompt).getByLabelText('Пароль')
    expect(password).toHaveFocus()

    // --- The user's work underneath is untouched -------------------------
    expect(formDialog).toHaveAttribute('open')
    expect(screen.getByLabelText(/Название/)).toHaveValue('BASIS')
    expect(screen.getByLabelText(/Пояснение/)).toHaveValue('черновик, который нельзя потерять')

    // Escape must not dismiss the re-login prompt (there is nothing to go
    // back to) and must not reach the dialog underneath either.
    await user.keyboard('{Escape}')
    expect(screen.getByRole('alertdialog')).toBeInTheDocument()
    expect(formDialog).toHaveAttribute('open')

    // --- Re-authenticate and finish the original action ------------------
    await user.type(password, 'pilot-secret')
    await user.click(within(prompt).getByRole('button', { name: 'Войти' }))

    await waitFor(() => expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument())
    expect(screen.getByLabelText(/Название/)).toHaveValue('BASIS')

    await user.click(screen.getByRole('button', { name: 'Создать' }))
    await waitFor(() => expect(screen.getByRole('heading', { name: 'BASIS' })).toBeInTheDocument())

    // The failed mutation was retried by the user, never silently replayed
    // by the client: exactly two POSTs, and the second carries the new token.
    const creates = calls.filter((c) => c.url === '/api/partners' && c.method === 'POST')
    expect(creates).toHaveLength(2)
    expect(creates[0]?.headers['x-csrf-token']).toBe('csrf-1')
    expect(creates[1]?.headers['x-csrf-token']).toBe('csrf-2')
  })
})

describe('explicit logout', () => {
  it('shows the ordinary login screen (not "session expired") and discards the retained draft', async () => {
    const user = userEvent.setup()
    const { calls } = renderApp()

    await openPartnerDraft(user)
    expect(screen.getByLabelText(/Название/)).toHaveValue('BASIS')

    await user.click(screen.getByRole('button', { name: 'Выйти' }))

    // Ordinary login screen: no expiry wording, no modal over preserved work.
    await waitFor(() => expect(screen.getByRole('button', { name: 'Войти' })).toBeInTheDocument())
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument()
    expect(screen.queryByText(/Сессия истекла/)).not.toBeInTheDocument()
    // The workspace — and with it the draft — is gone, not merely covered up.
    expect(screen.queryByLabelText(/Название/)).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: '+ Добавить партнёра' })).not.toBeInTheDocument()
    expect(calls.some((c) => c.url === '/api/session' && c.method === 'DELETE')).toBe(true)

    // Logging back in starts from a clean workspace: the previous draft must
    // not reappear.
    await user.type(screen.getByLabelText('Пароль'), 'pilot-secret')
    await user.click(screen.getByRole('button', { name: 'Войти' }))

    await waitFor(() =>
      expect(screen.getByRole('button', { name: '+ Добавить партнёра' })).toBeInTheDocument(),
    )
    expect(screen.queryByRole('heading', { name: 'Добавить партнёра' })).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: '+ Добавить партнёра' }))
    expect(screen.getByLabelText(/Название/)).toHaveValue('')
    expect(screen.getByLabelText(/Пояснение/)).toHaveValue('')
  })
})
