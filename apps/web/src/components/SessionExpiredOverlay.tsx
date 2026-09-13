import { useEffect, useId, useRef } from 'react'
import { LoginForm } from './LoginForm'

/**
 * Re-authentication prompt shown when the session expires mid-use (401),
 * *on top of* the still-mounted workspace instead of unmounting it: any
 * dialog/form the user had open underneath keeps its state, so logging back
 * in does not lose typed input.
 *
 * This must be a native `<dialog>` opened with `showModal()`, not a
 * positioned `<div>` with a big z-index. The workspace can legitimately have
 * a modal `<dialog>` already open (create/edit partner, upload) at the moment
 * the session dies — and a modal dialog is promoted to the browser's *top
 * layer*, which renders above every ordinary positioned element regardless of
 * z-index, while making everything outside it inert. A plain div prompt would
 * therefore be painted underneath that dialog and be unclickable/unfocusable:
 * the user could neither continue nor log back in.
 *
 * Because the top layer is a stack ordered by `showModal()` call time, this
 * dialog — shown last — sits above whatever was already open, and the browser
 * makes that earlier dialog inert for us (no hand-rolled focus trap needed).
 * The earlier dialog is never closed, so its form state survives untouched.
 */
export function SessionExpiredOverlay() {
  const titleId = useId()
  const ref = useRef<HTMLDialogElement>(null)

  useEffect(() => {
    const dialog = ref.current
    if (!dialog) return
    const previouslyFocused =
      document.activeElement instanceof HTMLElement ? document.activeElement : null

    if (!dialog.open) dialog.showModal()

    // Escape must not dismiss this one: there is nothing to return to — the
    // workspace underneath cannot do anything without a session, and closing
    // would leave an inert-looking page with no way back in.
    const handleCancel = (event: Event) => event.preventDefault()
    dialog.addEventListener('cancel', handleCancel)

    // `showModal()` moves focus itself (to the autofocus candidate, if the
    // browser finds one). React applies `autoFocus` by calling focus() during
    // commit — i.e. *before* this effect — so the password field is focused
    // explicitly here, after the dialog is actually open, rather than relying
    // on that ordering.
    dialog.querySelector<HTMLInputElement>('input[type="password"]')?.focus()

    return () => {
      dialog.removeEventListener('cancel', handleCancel)
      if (dialog.open) dialog.close()
      // Hand focus back to whatever the user was on when the session died —
      // typically a control inside the dialog that is still open underneath.
      previouslyFocused?.focus()
    }
  }, [])

  return (
    <dialog
      ref={ref}
      className="dialog dialog-session"
      aria-labelledby={titleId}
      // Native modal dialogs already expose role="dialog" + aria-modal, but
      // this one announces a state change the user did not ask for, so the
      // stronger alertdialog role is the accurate one.
      role="alertdialog"
    >
      <h2 id={titleId} className="visually-hidden">
        Сессия истекла
      </h2>
      <LoginForm
        autoFocus
        description="Сессия истекла. Введённые данные не потеряны — войдите ещё раз, чтобы продолжить."
      />
    </dialog>
  )
}
