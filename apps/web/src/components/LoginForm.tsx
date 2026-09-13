import { useId, useRef, useState, type FormEvent, type ReactNode } from 'react'
import { useAuth } from '../auth/AuthContext'

interface LoginFormProps {
  description?: ReactNode
  autoFocus?: boolean
}

/** The password form itself, shared between the full-screen LoginPage and the mid-session SessionExpiredOverlay. */
export function LoginForm({ description, autoFocus }: LoginFormProps) {
  const { login } = useAuth()
  const passwordId = useId()
  const errorId = useId()
  const inputRef = useRef<HTMLInputElement>(null)
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  async function handleSubmit(event: FormEvent) {
    event.preventDefault()
    if (password.length === 0) {
      setError('Введите пароль.')
      inputRef.current?.focus()
      return
    }
    setSubmitting(true)
    setError(null)
    try {
      await login(password)
      // Password is never retained after this point (not stored, not logged).
      setPassword('')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Не удалось войти.')
      setSubmitting(false)
      inputRef.current?.focus()
    }
  }

  return (
    <div className="login-card">
      <div className="login-card__brand">
        <span className="brand-mark" aria-hidden="true">
          <i></i>
          <i></i>
          <i></i>
        </span>
        OTDEL
      </div>
      {description ? <p style={{ color: 'var(--muted)', fontSize: '0.875rem', marginBottom: 8 }}>{description}</p> : null}
      <form onSubmit={handleSubmit} noValidate>
        <label className="field" htmlFor={passwordId}>
          <span>Пароль</span>
          <input
            ref={inputRef}
            id={passwordId}
            type="password"
            autoComplete="current-password"
            autoFocus={autoFocus}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            aria-invalid={error ? 'true' : undefined}
            aria-describedby={error ? errorId : undefined}
            disabled={submitting}
            required
          />
        </label>
        {error ? (
          <p className="field-error" id={errorId} role="alert">
            {error}
          </p>
        ) : null}
        <button type="submit" className="button button-primary" disabled={submitting} style={{ width: '100%' }}>
          {submitting ? 'Входим…' : 'Войти'}
        </button>
      </form>
    </div>
  )
}
