import type { User } from '../api/types'

// The bearer token lives in localStorage so deep links from Telegram open
// signed in. Wrapped in try/catch because private modes can throw.
const KEY = 'ferry.token'

let token: string | null = read()
const listeners = new Set<() => void>()

function read() {
  try {
    return localStorage.getItem(KEY)
  } catch {
    return null
  }
}

export const session = {
  get token() {
    return token
  },
  set(next: string | null) {
    token = next
    try {
      if (next) localStorage.setItem(KEY, next)
      else localStorage.removeItem(KEY)
    } catch {
      /* storage unavailable: keep the token in memory only */
    }
    listeners.forEach((l) => l())
  },
  subscribe(fn: () => void) {
    listeners.add(fn)
    return () => listeners.delete(fn)
  },
}

export type SessionUser = User
