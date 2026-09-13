import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import {
  ApiError,
  fetchSession,
  isCsrfError,
  isSessionExpiredError,
  login as apiLogin,
  logout as apiLogout,
} from '../api/client'

/**
 * Why "anonymous" is not a single, flat state.
 *
 * Not being logged in means two materially different things to the user, and
 * the UI must not conflate them:
 *
 *  - `initial`   — this tab has never had a session (first visit, or a
 *                  reload after the cookie was already gone). There is no
 *                  work in progress to protect: show the ordinary login
 *                  screen.
 *  - `expired`   — a session that *was* live died under the user's hands
 *                  (a 401 on a real request). There may be a half-filled
 *                  partner form or an upload dialog open right now, so the
 *                  workspace stays mounted and re-authentication happens on
 *                  top of it — nothing typed is lost.
 *  - `logged-out`— the user explicitly pressed "Выйти" and the server
 *                  confirmed it. Retaining their draft data on screen (and
 *                  telling them their session "expired") would be both wrong
 *                  and a privacy problem on a shared machine: the workspace
 *                  is torn down and the ordinary login screen is shown.
 *
 * A sticky "has been authenticated at least once" flag cannot express this:
 * it is true for both `expired` and `logged-out`.
 */
export type AnonymousReason = 'initial' | 'expired' | 'logged-out'

export type AuthState =
  | { status: 'checking' }
  | { status: 'anonymous'; reason: AnonymousReason }
  | { status: 'authenticated'; csrfToken: string }
  | { status: 'unreachable'; message: string }

/**
 * Thrown by `runMutation` when the session turned out to be expired (HTTP
 * 401). Distinct from a plain ApiError so callers can show
 * "session expired, log in again" copy instead of a generic failure — and,
 * crucially, so the UI can choose to keep whatever form the user was filling
 * in mounted (not lose the input) while prompting for the password again.
 */
export class SessionExpiredError extends Error {
  constructor() {
    super('Сессия истекла. Введите пароль ещё раз — уже введённые данные сохранены.')
    this.name = 'SessionExpiredError'
  }
}

interface AuthContextValue {
  state: AuthState
  /** The CSRF token to send with mutating requests, kept in memory only. */
  csrfToken: string | null
  login: (password: string) => Promise<void>
  logout: () => Promise<void>
  /** Re-checks GET /api/session, e.g. after a network error banner's retry button. */
  recheck: () => void
  /**
   * Runs one mutating API call with the current CSRF token, with a single,
   * narrowly-scoped recovery path:
   *  - `invalid_csrf_token` (403): the session is still valid but the token
   *    was stale, so GET /api/session is used to fetch a fresh one and `fn`
   *    is replayed exactly once with it. No further retries. If *that*
   *    refresh — or the single replay — comes back 401, the session really is
   *    gone and it is handled exactly like the plain-401 case below; a 403
   *    must never become an escape hatch that swallows a 401.
   *  - a real 401: the session is gone. State flips to
   *    `anonymous`/`expired` (shown as an overlay, not a full unmount — see
   *    App.tsx) and a `SessionExpiredError` is thrown so the caller's form is
   *    left intact.
   *  - anything else (validation errors, network errors, ...): rethrown
   *    as-is, with no retry — an ambiguous failure must never be silently
   *    replayed.
   */
  runMutation: <T>(fn: (csrfToken: string) => Promise<T>) => Promise<T>
}

const AuthContext = createContext<AuthContextValue | null>(null)

export function AuthProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<AuthState>({ status: 'checking' })
  const stateRef = useRef(state)
  stateRef.current = state

  const checkSession = useCallback(() => {
    setState({ status: 'checking' })
    fetchSession()
      .then((res) => {
        setState({ status: 'authenticated', csrfToken: res.csrf_token })
      })
      .catch((err: unknown) => {
        if (err instanceof ApiError && err.status === 401) {
          // A 401 on the *initial* check is not an expiry the user lived
          // through: there is no in-progress work on screen to preserve.
          setState({ status: 'anonymous', reason: 'initial' })
          return
        }
        const message =
          err instanceof Error ? err.message : 'Не удалось проверить сессию.'
        setState({ status: 'unreachable', message })
      })
  }, [])

  useEffect(() => {
    checkSession()
  }, [checkSession])

  const login = useCallback(async (password: string) => {
    const res = await apiLogin(password)
    setState({ status: 'authenticated', csrfToken: res.csrf_token })
  }, [])

  const logout = useCallback(async () => {
    const current = stateRef.current
    if (current.status !== 'authenticated') {
      setState({ status: 'anonymous', reason: 'logged-out' })
      return
    }
    // Only mark the session as ended once the server actually confirms it:
    // the HttpOnly cookie is still live if DELETE fails (network error, a
    // rejected CSRF token, ...), so forcing "anonymous" regardless would
    // just be a UI lie — the caller is expected to catch and surface this.
    await apiLogout(current.csrfToken)
    setState({ status: 'anonymous', reason: 'logged-out' })
  }, [])

  /**
   * Single place that turns "the server said 401" into the expired state.
   * Returns the error to throw, so every call site stays a `throw`.
   */
  const toExpiry = useCallback((err: unknown): unknown => {
    if (!isSessionExpiredError(err)) return err
    setState({ status: 'anonymous', reason: 'expired' })
    return new SessionExpiredError()
  }, [])

  const runMutation = useCallback(
    async <T,>(fn: (csrfToken: string) => Promise<T>): Promise<T> => {
      const current = stateRef.current
      if (current.status !== 'authenticated') {
        throw new SessionExpiredError()
      }
      try {
        return await fn(current.csrfToken)
      } catch (err) {
        if (!isCsrfError(err)) {
          throw toExpiry(err)
        }

        // Bounded, single recovery attempt: the 403 says the *token* was
        // stale, not that the session died. Both steps below can still
        // discover that it actually did die (401) — and both must then take
        // the same expiry path as a first-attempt 401, never surface as a
        // raw ApiError the caller would render as a generic failure.
        let fresh
        try {
          fresh = await fetchSession()
        } catch (refreshErr) {
          throw toExpiry(refreshErr)
        }
        setState({ status: 'authenticated', csrfToken: fresh.csrf_token })
        try {
          // Exactly one replay — no second recovery round, even if this
          // fails with `invalid_csrf_token` again.
          return await fn(fresh.csrf_token)
        } catch (replayErr) {
          throw toExpiry(replayErr)
        }
      }
    },
    [toExpiry],
  )

  const value = useMemo<AuthContextValue>(
    () => ({
      state,
      csrfToken: state.status === 'authenticated' ? state.csrfToken : null,
      login,
      logout,
      recheck: checkSession,
      runMutation,
    }),
    [state, login, logout, checkSession, runMutation],
  )

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>
}

export function useAuth(): AuthContextValue {
  const ctx = useContext(AuthContext)
  if (!ctx) {
    throw new Error('useAuth must be used within an AuthProvider')
  }
  return ctx
}
