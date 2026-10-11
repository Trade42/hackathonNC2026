import { getPrefs } from './prefs'

// Original water-drop sounds synthesized with Web Audio. No sample files.
// Off by default; the AudioContext is created on the first sound after the
// user turns sound on, which is always inside a user gesture.
let ctx: AudioContext | null = null

function audio() {
  if (!ctx) {
    const AC = window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
    if (!AC) return null
    ctx = new AC()
  }
  if (ctx.state === 'suspended') void ctx.resume()
  return ctx
}

function drop(at: number, from: number, to: number, gain = 0.18) {
  const a = audio()
  if (!a) return
  const t = a.currentTime + at
  const osc = a.createOscillator()
  const g = a.createGain()
  osc.type = 'sine'
  osc.frequency.setValueAtTime(from, t)
  osc.frequency.exponentialRampToValueAtTime(to, t + 0.07)
  g.gain.setValueAtTime(0.0001, t)
  g.gain.exponentialRampToValueAtTime(gain, t + 0.008)
  g.gain.exponentialRampToValueAtTime(0.0001, t + 0.17)
  osc.connect(g).connect(a.destination)
  osc.start(t)
  osc.stop(t + 0.2)
}

export const sound = {
  drop() {
    if (getPrefs().sound) drop(0, 880, 1320)
  },
  chime() {
    if (!getPrefs().sound) return
    drop(0, 880, 1320, 0.15)
    drop(0.09, 1320, 1980, 0.13)
  },
  // Used by the toggle itself so turning sound on gives immediate feedback.
  preview() {
    drop(0, 880, 1320, 0.12)
  },
}
