import { motion, MotionConfig } from 'motion/react'
import { useSyncExternalStore } from 'react'
import { useReducedMotion } from '../lib/prefs'
import { popStore, type Pop } from './pops'

function Burst({ p }: { p: Pop }) {
  const reduced = useReducedMotion()
  const color = p.tone === 'leaf' ? '92,194,74' : '30,143,214'
  const drops = reduced ? [] : Array.from({ length: 8 }, (_, i) => (i / 8) * Math.PI * 2 + 0.3)
  return (
    <div style={{ position: 'fixed', left: p.x, top: p.y, width: 0, height: 0, pointerEvents: 'none' }}>
      {!reduced && (
        <motion.span
          initial={{ scale: 0.4, opacity: 0.9 }}
          animate={{ scale: [0.4, 1.18, 1.3], opacity: [0.9, 0.9, 0] }}
          transition={{ duration: 0.35, times: [0, 0.6, 1], ease: [0.2, 0.9, 0.25, 1] }}
          onAnimationComplete={() => !p.label && popStore.remove(p.id)}
          style={{
            position: 'absolute',
            left: -p.size / 2,
            top: -p.size / 2,
            width: p.size,
            height: p.size,
            borderRadius: '50%',
            border: '2px solid rgba(255,255,255,.95)',
            background: `radial-gradient(circle at 32% 28%, #fff 0 8%, rgba(255,255,255,.4) 16%, rgba(${color},.25) 60%, rgba(${color},.5) 100%)`,
            boxShadow: `0 0 0 1px rgba(${color},.35)`,
          }}
        />
      )}
      {drops.map((a, i) => {
        const d = 22 + (i % 3) * 9 + p.size / 3
        return (
          <motion.span
            key={i}
            initial={{ x: 0, y: 0, scale: 1, opacity: 1 }}
            animate={{ x: Math.cos(a) * d, y: Math.sin(a) * d, scale: 0.3, opacity: 0 }}
            transition={{ duration: 0.32, delay: 0.08, ease: [0.2, 0.9, 0.25, 1] }}
            style={{
              position: 'absolute',
              left: -4,
              top: -4,
              width: 8,
              height: 8,
              borderRadius: '50%',
              background: `radial-gradient(circle at 35% 30%, #fff 0 25%, rgba(${color},.85) 70%)`,
            }}
          />
        )
      })}
      {p.label && (
        <motion.span
          initial={{ y: 0, opacity: 0, scale: 0.9 }}
          animate={reduced ? { opacity: [0, 1, 1, 0] } : { y: [-4, -44, -52, -60], opacity: [0, 1, 1, 0], scale: [0.9, 1.05, 1, 1] }}
          transition={reduced ? { duration: 1.4, times: [0, 0.1, 0.8, 1] } : { duration: 1.3, times: [0, 0.25, 0.8, 1], ease: [0.2, 0.9, 0.25, 1] }}
          onAnimationComplete={() => popStore.remove(p.id)}
          style={{
            position: 'absolute',
            left: 0,
            top: -14,
            whiteSpace: 'nowrap',
            padding: '4px 12px',
            borderRadius: 999,
            background: 'var(--panel-solid)',
            color: p.tone === 'leaf' ? 'var(--leaf-700)' : 'var(--sky-700)',
            fontWeight: 600,
            fontSize: 'var(--fs-sm)',
            boxShadow: 'var(--shadow-panel)',
            translate: '-50% 0',
          }}
        >
          {p.label}
        </motion.span>
      )}
    </div>
  )
}

export default function PopLayer() {
  const pops = useSyncExternalStore(popStore.subscribe, popStore.get)
  const reduced = useReducedMotion()
  return (
    <MotionConfig reducedMotion={reduced ? 'always' : 'never'}>
    <div aria-hidden data-pop-layer style={{ position: 'fixed', inset: 0, pointerEvents: 'none', zIndex: 'var(--z-celebrate)' as unknown as number }}>
      {pops.map((p) => (
        <Burst key={p.id} p={p} />
      ))}
    </div>
    </MotionConfig>
  )
}
