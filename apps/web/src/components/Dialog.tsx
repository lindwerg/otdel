import { useEffect, useRef, type ReactNode } from 'react'

interface DialogProps {
  open: boolean
  onClose: () => void
  labelledBy: string
  className?: string
  children: ReactNode
  /**
   * When true, blocks the native Escape-to-close default action (via
   * `cancel` event `preventDefault()`) so an in-flight submit/upload cannot
   * be closed out from under itself and left unreopenable. The explicit
   * "Готово"/"Отмена" buttons are expected to stay disabled at the same time.
   */
  preventClose?: boolean
}

/**
 * Thin wrapper around the native <dialog> element.
 *
 * Native <dialog showModal()> already gives us, for free, everything the
 * accessibility requirements ask for: a top-layer focus trap, Escape-to-close
 * (fires 'cancel' then 'close'), and inert background content. The only
 * thing left to do by hand is returning focus to whatever element triggered
 * the dialog — captured here as `document.activeElement` at the moment
 * showModal() is called, which is exactly the button the user actually
 * activated (not a guessed/static element).
 */
export function Dialog({ open, onClose, labelledBy, className, children, preventClose = false }: DialogProps) {
  const ref = useRef<HTMLDialogElement>(null)
  const triggerRef = useRef<HTMLElement | null>(null)
  const preventCloseRef = useRef(preventClose)
  preventCloseRef.current = preventClose
  // Latest-ref, not a `[onClose]` effect dependency: none of this
  // component's real callers pass a memoized `onClose` (every call site is
  // an inline `() => setXOpen(false)`), so a fresh identity arrives on
  // *every* render — including the very render that flips `open` to false.
  // React tears down + re-registers dependent effects strictly in two
  // separate passes across the whole commit (all cleanups, then all new
  // effects), so if the close-listener effect depended on `onClose`, its
  // *old* listener would already be removed (cleanup phase) before the
  // *new* one is attached (effect phase) — and the open/close effect below
  // runs its `dialog.close()` in between those two passes, dispatching the
  // native 'close' event into a brief window with no listener attached at
  // all. The result: the dialog visually closes but onClose()/focus-return
  // silently never fire. Keeping the listener itself mount-once (deps `[]`)
  // and reading `onCloseRef.current` inside it avoids that window entirely.
  const onCloseRef = useRef(onClose)
  onCloseRef.current = onClose

  useEffect(() => {
    const dialog = ref.current
    if (!dialog) return
    if (open && !dialog.open) {
      triggerRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null
      dialog.showModal()
    } else if (!open && dialog.open) {
      dialog.close()
    }
  }, [open])

  useEffect(() => {
    const dialog = ref.current
    if (!dialog) return
    const handleClose = () => {
      onCloseRef.current()
      triggerRef.current?.focus()
    }
    // 'cancel' fires first (Escape's default action) and is cancelable: if a
    // submit/upload is in flight, block it here so the native dialog never
    // closes itself out from under a busy operation (see `preventClose`).
    const handleCancel = (event: Event) => {
      if (preventCloseRef.current) {
        event.preventDefault()
      }
    }
    dialog.addEventListener('close', handleClose)
    dialog.addEventListener('cancel', handleCancel)
    return () => {
      dialog.removeEventListener('close', handleClose)
      dialog.removeEventListener('cancel', handleCancel)
    }
    // Mount-once: see the onCloseRef comment above for why `onClose` must
    // NOT be a dependency here.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  return (
    <dialog ref={ref} className={`dialog${className ? ` ${className}` : ''}`} aria-labelledby={labelledBy}>
      {open ? children : null}
    </dialog>
  )
}
