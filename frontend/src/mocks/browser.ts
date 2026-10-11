import { setupWorker } from 'msw/browser'
import { handlers, wsHandler } from './handlers'
import { reset, skipStep, tick, triggerInvite } from './db'

export async function startMocks() {
  const worker = setupWorker(...handlers, wsHandler)
  await worker.start({
    onUnhandledFrame: 'bypass',
    quiet: true,
    serviceWorker: { url: `${import.meta.env.BASE_URL}mockServiceWorker.js` },
  })
  // Test and rehearsal hooks (mock mode only).
  ;(window as unknown as { __ferryMock: unknown }).__ferryMock = { reset, skipStep, triggerInvite }
  // The demo clock: closes the window, buys, picks up and walks the route.
  tick()
  setInterval(() => tick(), 1000)
}
