import { isReducedMotion } from '../lib/prefs'
import { sound } from '../lib/sound'
import { vibrate } from '../platform'

export type Pop = { id: number; x: number; y: number; label?: string; tone: 'sky' | 'leaf'; size: number }

let seq = 0
let pops: Pop[] = []
const subs = new Set<() => void>()

export const popStore = {
  get: () => pops,
  subscribe(fn: () => void) {
    subs.add(fn)
    return () => subs.delete(fn)
  },
  remove(id: number) {
    pops = pops.filter((p) => p.id !== id)
    subs.forEach((s) => s())
  },
}

type PopOpts = { label?: string; tone?: 'sky' | 'leaf'; size?: number; sound?: 'drop' | 'chime' }

// Pop a bubble at an element (or the viewport center). Only call this for the
// five real events listed in DESIGN.md.
export function pop(target: Element | { x: number; y: number } | null, opts: PopOpts = {}) {
  let x = window.innerWidth / 2
  let y = window.innerHeight / 2
  if (target instanceof Element) {
    const r = target.getBoundingClientRect()
    x = r.left + r.width / 2
    y = r.top + r.height / 2
  } else if (target) {
    x = target.x
    y = target.y
  }
  pops = [...pops, { id: ++seq, x, y, label: opts.label, tone: opts.tone ?? 'sky', size: opts.size ?? 56 }]
  subs.forEach((s) => s())
  if (opts.sound === 'chime') sound.chime()
  else sound.drop()
  if (!isReducedMotion()) vibrate(12)
}
