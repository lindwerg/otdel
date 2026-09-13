import { Navigate, Route, Routes } from 'react-router-dom'
import { useAuth } from './auth/AuthContext'
import { SessionExpiredOverlay } from './components/SessionExpiredOverlay'
import { StatusMessage } from './components/StatusMessage'
import { LoginPage } from './pages/LoginPage'
import { WorkspacePage } from './pages/WorkspacePage'

// Routing is intentionally minimal for phase 1A (only the materials
// workspace exists), but structured as <Routes> so later phases (1B+) can
// add sibling routes (e.g. knowledge/gaps/versions) without reshaping this
// file — see docs/implementation-contract.md "Следующие контракты".
export function App() {
  const { state, recheck } = useAuth()

  if (state.status === 'checking') {
    return (
      <div className="login-screen">
        <StatusMessage>Проверяем сеанс…</StatusMessage>
      </div>
    )
  }

  if (state.status === 'unreachable') {
    return (
      <div className="login-screen">
        <StatusMessage tone="error" onRetry={recheck}>
          {state.message}
        </StatusMessage>
      </div>
    )
  }

  // Not logged in, and nothing of the user's is at stake: either this tab
  // never had a session, or the user deliberately logged out. In both cases
  // the workspace is fully unmounted — which is exactly what discards any
  // retained draft/upload state — and the ordinary login screen is shown.
  // Only a session that expired mid-use takes the other branch.
  const sessionExpired = state.status === 'anonymous' && state.reason === 'expired'
  if (state.status === 'anonymous' && !sessionExpired) {
    return <LoginPage />
  }

  // Authenticated, or expired mid-use. In the expired case the workspace
  // stays mounted and re-authentication happens in a modal on top of it, so
  // any dialog/form the user had open (partner draft, in-progress upload
  // list) keeps its state — see auth/AuthContext.tsx's `AnonymousReason`,
  // `SessionExpiredError` and `runMutation` doc comments.
  return (
    <>
      <Routes>
        <Route path="/" element={<WorkspacePage />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
      {sessionExpired ? <SessionExpiredOverlay /> : null}
    </>
  )
}
