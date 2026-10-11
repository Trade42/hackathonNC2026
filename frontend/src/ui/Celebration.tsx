import { Leaf } from '@phosphor-icons/react'
import { AnimatePresence, motion } from 'motion/react'
import { useEffect, useRef } from 'react'
import { Link } from 'react-router-dom'
import { grams } from '../lib/format'
import { useReducedMotion } from '../lib/prefs'
import { sound } from '../lib/sound'
import { vibrate } from '../platform'
import { GelButton } from './GelButton'

// The delivery lands: a bubble bursts into the CO2 it saved.
export function Celebration({ open, grams: g, onClose }: { open: boolean; grams: number; onClose: () => void }) {
  const reduced = useReducedMotion()
  const btn = useRef<HTMLButtonElement>(null)
  useEffect(() => {
    if (!open) return
    sound.chime()
    if (!reduced) vibrate([10, 40, 18])
    const t = window.setTimeout(() => btn.current?.focus(), 400)
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', onKey)
    return () => {
      window.clearTimeout(t)
      window.removeEventListener('keydown', onKey)
    }
  }, [open, reduced, onClose])

  const drops = reduced ? [] : Array.from({ length: 14 }, (_, i) => (i / 14) * Math.PI * 2)
  return (
    <AnimatePresence>
      {open && (
        <motion.div
          role="dialog"
          aria-modal="true"
          aria-labelledby="celebrate-h"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.18 }}
          style={{
            position: 'fixed',
            inset: 0,
            zIndex: 60,
            display: 'grid',
            placeItems: 'center',
            padding: 24,
            background: 'radial-gradient(circle at 50% 45%, rgba(238,249,233,.92), rgba(10,60,90,.45))',
          }}
          onClick={(e) => e.target === e.currentTarget && onClose()}
        >
          <div style={{ position: 'relative', display: 'grid', justifyItems: 'center', gap: 16, textAlign: 'center' }}>
            <div style={{ position: 'relative', width: 180, height: 180 }}>
              {!reduced && (
                <motion.span
                  aria-hidden
                  initial={{ scale: 0.3, opacity: 1 }}
                  animate={{ scale: [0.3, 1.12, 1.4], opacity: [1, 1, 0] }}
                  transition={{ duration: 0.35, times: [0, 0.65, 1], ease: [0.2, 0.9, 0.25, 1] }}
                  style={{
                    position: 'absolute',
                    inset: 0,
                    borderRadius: '50%',
                    border: '3px solid #fff',
                    background: 'radial-gradient(circle at 32% 28%, #fff 0 8%, rgba(255,255,255,.4) 16%, rgba(92,194,74,.25) 60%, rgba(46,154,59,.55) 100%)',
                  }}
                />
              )}
              {drops.map((a, i) => (
                <motion.span
                  key={i}
                  aria-hidden
                  initial={{ x: 0, y: 0, opacity: 1, scale: 1 }}
                  animate={{ x: Math.cos(a) * (110 + (i % 3) * 18), y: Math.sin(a) * (110 + (i % 3) * 18), opacity: 0, scale: 0.4 }}
                  transition={{ duration: 0.5, delay: 0.25, ease: [0.2, 0.9, 0.25, 1] }}
                  style={{
                    position: 'absolute',
                    left: 84,
                    top: 84,
                    width: 12,
                    height: 12,
                    borderRadius: '50%',
                    background: 'radial-gradient(circle at 35% 30%, #fff 0 25%, rgba(92,194,74,.9) 70%)',
                  }}
                />
              ))}
              <motion.div
                initial={reduced ? { opacity: 0 } : { scale: 0.6, opacity: 0 }}
                animate={reduced ? { opacity: 1 } : { scale: 1, opacity: 1 }}
                transition={reduced ? { duration: 0.15 } : { type: 'spring', stiffness: 700, damping: 18, delay: 0.28 }}
                style={{
                  position: 'absolute',
                  inset: 18,
                  display: 'grid',
                  placeItems: 'center',
                  borderRadius: '50%',
                  background: 'radial-gradient(circle at 32% 26%, #fff 0 6%, rgba(255,255,255,.55) 12%, transparent 30%), radial-gradient(circle at 50% 45%, #7fd36f, #2e9a3b 78%, #1c6b2a)',
                  border: '2px solid #fff',
                  boxShadow: '0 18px 40px rgba(23,97,31,.35), inset 0 -8px 16px rgba(0,40,0,.25)',
                  color: '#fff',
                }}
              >
                <Leaf size={56} weight="fill" aria-hidden />
              </motion.div>
            </div>
            <motion.div
              initial={{ y: 12, opacity: 0 }}
              animate={{ y: 0, opacity: 1 }}
              transition={{ type: 'spring', stiffness: 520, damping: 34, delay: reduced ? 0 : 0.4 }}
              style={{ display: 'grid', gap: 6, padding: '18px 22px', borderRadius: 22, background: 'var(--panel-solid)', boxShadow: 'var(--shadow-lift)', maxWidth: 360 }}
            >
              <h2 id="celebrate-h" className="co2 num" style={{ fontSize: 'var(--fs-3xl)', fontWeight: 300 }}>
                +{grams(g)} CO2 saved
              </h2>
              <p style={{ color: 'var(--ink)' }}>vs a car delivery (EPA estimate)</p>
              <div style={{ display: 'flex', gap: 12, justifyContent: 'center', flexWrap: 'wrap', marginTop: 8 }}>
                <GelButton ref={btn} variant="eco" onClick={onClose}>
                  Enjoy your food
                </GelButton>
                <Link to="/impact" style={{ alignSelf: 'center', fontWeight: 600, minHeight: 44, display: 'inline-flex', alignItems: 'center' }}>
                  See my impact
                </Link>
              </div>
            </motion.div>
          </div>
        </motion.div>
      )}
    </AnimatePresence>
  )
}
