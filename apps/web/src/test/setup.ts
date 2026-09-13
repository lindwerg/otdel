import '@testing-library/jest-dom/vitest'
import { cleanup } from '@testing-library/react'
import { afterEach } from 'vitest'

// React Testing Library's auto-cleanup registers itself onto a *global*
// `afterEach` (present when `test.globals: true`); this project imports test
// hooks explicitly per-file instead, so that global never exists and RTL's
// auto-registration silently no-ops. Without this, DOM from one test leaks
// into the next within the same file (e.g. two <dialog>s both matching a
// query), so it is wired up explicitly here instead.
afterEach(() => {
  cleanup()
})

// jsdom implements the <dialog> *element* (attribute reflection etc.) but
// not showModal()/close(), and it never simulates the browser's own default
// action of closing the top modal <dialog> on Escape. This shim exists only
// for the test environment (this file is a Vitest setupFile, never imported
// by application code / the production bundle) so component tests can
// exercise the same open → Escape/close → focus-return contract that real
// browsers already provide natively for <dialog>.
if (typeof HTMLDialogElement !== 'undefined' && !HTMLDialogElement.prototype.showModal) {
  // Stand-in for the browser's top layer: a stack ordered by showModal()
  // call time, where the last entry is the dialog actually on top. Modelling
  // this (rather than just "the first dialog[open] in document order")
  // matters as soon as one modal is opened above another — e.g. the
  // session-expiry prompt over an already-open partner/upload dialog — since
  // document order and stacking order then disagree.
  const modalStack: HTMLDialogElement[] = []

  HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
    if (this.hasAttribute('open')) return
    this.setAttribute('open', '')
    modalStack.push(this)
  }
  HTMLDialogElement.prototype.close = function (this: HTMLDialogElement, returnValue?: string) {
    if (!this.hasAttribute('open')) return
    this.removeAttribute('open')
    const index = modalStack.indexOf(this)
    if (index >= 0) modalStack.splice(index, 1)
    if (returnValue !== undefined) this.returnValue = returnValue
    this.dispatchEvent(new Event('close'))
  }

  document.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape') return
    // Drop dialogs that were unmounted while still open (React removes the
    // element without calling close()); the browser likewise evicts a
    // removed element from the top layer.
    while (modalStack.length > 0) {
      const candidate = modalStack[modalStack.length - 1]
      if (candidate && candidate.isConnected) break
      modalStack.pop()
    }
    // Escape addresses the topmost modal only — never one buried underneath.
    const topDialog = modalStack[modalStack.length - 1]
    if (!topDialog) return
    const cancelEvent = new Event('cancel', { cancelable: true })
    topDialog.dispatchEvent(cancelEvent)
    // Mirrors the real browser default action: only auto-close if nothing
    // called preventDefault() on 'cancel' (e.g. Dialog's `preventClose`).
    if (!cancelEvent.defaultPrevented) {
      topDialog.close()
    }
  })
}
