const mode = (import.meta.env.VITE_API_MODE ?? 'mock') as 'mock' | 'live'

const trim = (s: string) => s.replace(/\/+$/, '')

// Mock mode pins its own bases (MSW intercepts them), so a live URL left in
// .env can never be reached from the mock demo.
const isMock = mode === 'mock'

export const env = {
  mode,
  isMock,
  apiBase: isMock ? 'https://api.ferry.mock' : trim(import.meta.env.VITE_API_BASE_URL ?? 'https://api.ferry.mock'),
  // The stream's base: realtime.ts appends /v1/stream.
  wsBase: isMock ? 'wss://api.ferry.mock' : trim(import.meta.env.VITE_WS_URL ?? 'wss://api.ferry.mock'),
} as const
