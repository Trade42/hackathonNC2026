import './styles/tokens.css'
import './styles/base.css'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { BrowserRouter } from 'react-router-dom'
import { ApiError, setReady } from './api/client'
import { realtime } from './api/realtime'
import { env } from './config/env'
import App from './App'

const qc = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 15_000,
      refetchOnWindowFocus: true,
      // Retry network blips, not answers like 404 or 403.
      retry: (count, err) => (err instanceof ApiError && err.status >= 400 && err.status < 500 ? false : count < 2),
    },
    mutations: { retry: false },
  },
})
realtime.attach(qc)

function boot() {
  // Render right away; in mock mode requests wait until the mock worker is up.
  // On the landing page the mocks load after the page does, so they never
  // compete with the first paint.
  if (env.isMock) {
    const start = () => import('./mocks/browser').then((m) => m.startMocks())
    const onLanding = window.location.pathname === '/'
    setReady(
      onLanding
        ? new Promise((resolve) => {
            const go = () => setTimeout(() => resolve(start()), 200)
            if (document.readyState === 'complete') go()
            else window.addEventListener('load', go, { once: true })
          })
        : start(),
    )
  }
  createRoot(document.getElementById('root')!).render(
    <StrictMode>
      <QueryClientProvider client={qc}>
        <BrowserRouter>
          <App />
        </BrowserRouter>
      </QueryClientProvider>
    </StrictMode>,
  )
}

boot()
