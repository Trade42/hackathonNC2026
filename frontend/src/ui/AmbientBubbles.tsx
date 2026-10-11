import { useEffect, useRef } from 'react'
import { useReducedMotion } from '../lib/prefs'

type Props = { count: number; className?: string }

// One slow rising bubble per real delivery this week (capped at 18). A single
// canvas at 30 fps; pauses when the tab is hidden or the canvas is off-screen,
// and renders nothing under reduced motion.
export function AmbientBubbles({ count, className }: Props) {
  const ref = useRef<HTMLCanvasElement>(null)
  const reduced = useReducedMotion()
  const n = Math.min(18, Math.max(0, Math.round(count)))

  useEffect(() => {
    const canvas = ref.current
    if (!canvas || reduced || n === 0) return
    const ctx = canvas.getContext('2d')
    if (!ctx) return
    let w = 0
    let h = 0
    const dpr = Math.min(window.devicePixelRatio || 1, 2)
    const resize = () => {
      const r = canvas.getBoundingClientRect()
      w = r.width
      h = r.height
      canvas.width = Math.round(w * dpr)
      canvas.height = Math.round(h * dpr)
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
    }
    resize()
    const bubbles = Array.from({ length: n }, (_, i) => ({
      x: ((i * 0.618) % 1) * 0.9 + 0.05,
      y: Math.random(),
      r: 4 + ((i * 7) % 9),
      speed: 0.018 + ((i * 13) % 10) / 1000,
      wobble: Math.random() * Math.PI * 2,
    }))
    let raf = 0
    let last = 0
    let visible = true
    let onScreen = true
    const frame = (t: number) => {
      raf = requestAnimationFrame(frame)
      if (t - last < 33) return
      const dt = Math.min(0.1, (t - last) / 1000)
      last = t
      ctx.clearRect(0, 0, w, h)
      for (const b of bubbles) {
        b.y -= b.speed * dt * (1 + b.r / 12)
        b.wobble += dt * 1.4
        if (b.y < -0.08) {
          b.y = 1.08
          b.x = Math.random() * 0.9 + 0.05
        }
        const x = b.x * w + Math.sin(b.wobble) * 6
        const y = b.y * h
        const g = ctx.createRadialGradient(x - b.r * 0.35, y - b.r * 0.4, b.r * 0.1, x, y, b.r)
        g.addColorStop(0, 'rgba(255,255,255,0.95)')
        g.addColorStop(0.35, 'rgba(255,255,255,0.25)')
        g.addColorStop(1, 'rgba(124,200,242,0.45)')
        ctx.beginPath()
        ctx.arc(x, y, b.r, 0, Math.PI * 2)
        ctx.fillStyle = g
        ctx.fill()
        ctx.lineWidth = 1
        ctx.strokeStyle = 'rgba(255,255,255,0.8)'
        ctx.stroke()
      }
    }
    const run = () => {
      cancelAnimationFrame(raf)
      if (visible && onScreen) {
        last = performance.now()
        raf = requestAnimationFrame(frame)
      }
    }
    const onVis = () => {
      visible = !document.hidden
      run()
    }
    const io = new IntersectionObserver(([e]) => {
      onScreen = e.isIntersecting
      run()
    })
    io.observe(canvas)
    const ro = new ResizeObserver(resize)
    ro.observe(canvas)
    document.addEventListener('visibilitychange', onVis)
    run()
    return () => {
      cancelAnimationFrame(raf)
      io.disconnect()
      ro.disconnect()
      document.removeEventListener('visibilitychange', onVis)
    }
  }, [n, reduced])

  if (reduced || n === 0) return null
  return <canvas ref={ref} className={className} aria-hidden style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', pointerEvents: 'none' }} />
}
