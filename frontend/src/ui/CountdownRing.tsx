import type { ReactNode } from 'react'
import { useNow } from '../lib/time'
import { countdown } from '../lib/format'

type Props = { closesAt: string; windowMs?: number; size?: number; children: ReactNode; label: string }

// The ring is the time left in the ordering window; it drains as it closes.
export function CountdownRing({ closesAt, windowMs = 15 * 60e3, size = 76, children, label }: Props) {
  const now = useNow(1000)
  const left = Math.max(0, Date.parse(closesAt) - now)
  const frac = Math.min(1, left / windowMs)
  const stroke = 4
  const r = (size - stroke) / 2
  const c = 2 * Math.PI * r
  const urgent = left < 2 * 60e3
  return (
    <span style={{ position: 'relative', display: 'inline-grid', placeItems: 'center', width: size, height: size }}>
      <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} style={{ position: 'absolute', inset: 0, transform: 'rotate(-90deg)' }} aria-hidden>
        <circle cx={size / 2} cy={size / 2} r={r} fill="none" stroke="rgba(255,255,255,.75)" strokeWidth={stroke} />
        <circle
          cx={size / 2}
          cy={size / 2}
          r={r}
          fill="none"
          stroke={urgent ? '#C27C00' : 'var(--sky-500)'}
          strokeWidth={stroke}
          strokeLinecap="round"
          strokeDasharray={c}
          strokeDashoffset={c * (1 - frac)}
          style={{ transition: 'stroke-dashoffset 1s linear' }}
        />
      </svg>
      {children}
      <span className="sr-only">
        {label}: {left > 0 ? `${countdown(left)} left to order` : 'window closed'}
      </span>
    </span>
  )
}
