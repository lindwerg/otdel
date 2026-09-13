import type { ReactNode } from 'react'

interface StatusMessageProps {
  tone?: 'neutral' | 'warn' | 'error'
  children: ReactNode
  onRetry?: () => void
  retryLabel?: string
}

/** A single reusable loading/empty/error line — never a fake percentage or ETA. */
export function StatusMessage({ tone = 'neutral', children, onRetry, retryLabel }: StatusMessageProps) {
  const className =
    tone === 'error'
      ? 'status-message status-message--error'
      : tone === 'warn'
        ? 'status-message status-message--warn'
        : 'status-message'
  return (
    <div className={className} role={tone === 'error' ? 'alert' : 'status'}>
      <p>{children}</p>
      {onRetry ? (
        <button type="button" className="button button-outline button-small" onClick={onRetry}>
          {retryLabel ?? 'Повторить'}
        </button>
      ) : null}
    </div>
  )
}
