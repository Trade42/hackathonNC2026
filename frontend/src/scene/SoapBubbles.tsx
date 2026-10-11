import { useEffect, useRef, useState, type CSSProperties } from 'react'
import { sound } from '../lib/sound'
import s from './SoapBubbles.module.css'

// Big glassy soap bubbles that fade in, drift, lean away from the pointer and
// pop when clicked (with the water-drop sound if it's on). Decorative, so they
// are hidden from assistive tech and never take focus.

export type BubbleSpot = { x: string; y: string; size: string; depth: number; delay: number; drift?: number }

const HERO: BubbleSpot[] = [
  { x: '81%', y: '24%', size: 'clamp(84px, 13vw, 210px)', depth: 1.3, delay: 0.5, drift: 13 },
  { x: '8%', y: '30%', size: 'clamp(54px, 8.5vw, 132px)', depth: 0.9, delay: 0.75, drift: 11 },
  { x: '67%', y: '50%', size: 'clamp(38px, 6vw, 92px)', depth: 0.6, delay: 0.95, drift: 9 },
  { x: '5%', y: '80%', size: 'clamp(84px, 14vw, 214px)', depth: 1.5, delay: 1.05, drift: 15 },
  { x: '90%', y: '64%', size: 'clamp(26px, 3.6vw, 58px)', depth: 0.5, delay: 1.25, drift: 8 },
  { x: '28%', y: '60%', size: 'clamp(22px, 2.8vw, 44px)', depth: 0.4, delay: 1.4, drift: 10 },
]

export function SoapBubbles({ spots = HERO, className }: { spots?: BubbleSpot[]; className?: string }) {
  const [popped, setPopped] = useState<Record<number, number>>({})
  const timers = useRef<number[]>([])
  useEffect(() => () => timers.current.forEach(clearTimeout), [])

  const pop = (i: number) => {
    if (popped[i]) return
    sound.drop()
    setPopped((p) => ({ ...p, [i]: Date.now() }))
    // A fresh bubble floats back in a little later.
    timers.current.push(
      window.setTimeout(() => {
        setPopped((p) => {
          const next = { ...p }
          delete next[i]
          return next
        })
      }, 4200),
    )
  }

  return (
    <div className={`${s.field} ${className ?? ''}`} aria-hidden>
      {spots.map((b, i) => {
        const isPopped = !!popped[i]
        return (
          <div
            key={`${i}-${isPopped ? 'p' : 'b'}`}
            className={s.spot}
            style={{ left: b.x, top: b.y, width: b.size, '--depth': b.depth } as CSSProperties}
          >
            {isPopped ? (
              <div className={s.burst}>
                {Array.from({ length: 8 }, (_, k) => (
                  <span key={k} style={{ '--a': `${k * 45}deg` } as CSSProperties} />
                ))}
              </div>
            ) : (
              <div className={s.enter} style={{ animationDelay: `${b.delay}s` }}>
                <div className={s.float} style={{ animationDuration: `${b.drift ?? 12}s`, animationDelay: `${-i * 1.7}s` }}>
                  <div className={s.bubble} onPointerDown={() => pop(i)} />
                </div>
              </div>
            )}
          </div>
        )
      })}
    </div>
  )
}
