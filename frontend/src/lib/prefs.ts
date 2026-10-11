import { useSyncExternalStore } from 'react'

export type Prefs = { sound: boolean; motion: 'system' | 'reduced' }
const KEY = 'ferry.prefs'
const defaults: Prefs = { sound: false, motion: 'system' }

let prefs: Prefs = read()
const subs = new Set<() => void>()

function read(): Prefs {
  try {
    return { ...defaults, ...JSON.parse(localStorage.getItem(KEY) ?? '{}') }
  } catch {
    return defaults
  }
}

export function setPrefs(patch: Partial<Prefs>) {
  prefs = { ...prefs, ...patch }
  try {
    localStorage.setItem(KEY, JSON.stringify(prefs))
  } catch {
    /* in-memory only */
  }
  subs.forEach((s) => s())
}

export const getPrefs = () => prefs

const subscribe = (fn: () => void) => {
  subs.add(fn)
  return () => subs.delete(fn)
}

export const usePrefs = () => useSyncExternalStore(subscribe, () => prefs)

const mq = typeof window !== 'undefined' ? window.matchMedia('(prefers-reduced-motion: reduce)') : null
const subscribeMq = (fn: () => void) => {
  mq?.addEventListener('change', fn)
  const off = subscribe(fn)
  return () => {
    mq?.removeEventListener('change', fn)
    off()
  }
}

export const isReducedMotion = () => prefs.motion === 'reduced' || !!mq?.matches

// True when the system asks for reduced motion or the user turned it down in Settings.
export const useReducedMotion = () => useSyncExternalStore(subscribeMq, isReducedMotion)
