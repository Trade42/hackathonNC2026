import { useEffect, useRef } from 'react'
import { isReducedMotion } from '../lib/prefs'
import s from './Scenery.module.css'

// Original Frutiger Aero scenery drawn in code: sky, cumulus clouds with a
// halftone shade, a meadow, the sea and a little ferry boat. Every layer that
// moves is its own element, so the browser composites it instead of
// re-running the SVG filters on each frame. Decorative only.

type CloudSpec = { cls: string; seed: number; flip?: boolean }

const CLOUDS: Record<'full' | 'compact', CloudSpec[]> = {
  full: [
    { cls: s.cHighL, seed: 7 },
    { cls: s.cBackL, seed: 3 },
    { cls: s.cBackR, seed: 11, flip: true },
    { cls: s.cFrontL, seed: 5 },
    { cls: s.cFrontR, seed: 9, flip: true },
  ],
  compact: [
    { cls: s.cBackL, seed: 3 },
    { cls: s.cFrontR, seed: 9, flip: true },
  ],
}

// One cumulus: a soft blue underside, shaded lobes, a halftone print in the
// shadows and a sunlit top, all billowed by one turbulence filter.
const PUFFS: [number, number, number][] = [
  [62, 158, 44],
  [108, 128, 58],
  [168, 100, 74],
  [236, 86, 70],
  [292, 112, 62],
  [342, 146, 46],
  [140, 162, 52],
  [214, 156, 60],
  [286, 164, 50],
  [370, 172, 30],
  [30, 176, 28],
  [200, 128, 66],
]

function Cloud({ cls, seed, flip }: CloudSpec) {
  const f = `url(#aero-billow-${seed % 3})`
  return (
    <div className={`${s.cloud} ${cls}`}>
      <svg viewBox="0 0 400 220" preserveAspectRatio="xMidYMid meet" style={flip ? { transform: 'scaleX(-1)' } : undefined}>
        <g filter={f}>
          <g fill="#a6c4e2" filter="url(#aero-soft)">
            {PUFFS.map(([x, y, r], i) => (
              <circle key={i} cx={x + 4} cy={y + 16} r={r} />
            ))}
          </g>
          <g fill="url(#aero-puff)">
            {PUFFS.map(([x, y, r], i) => (
              <circle key={i} cx={x} cy={y} r={r} />
            ))}
          </g>
          <g fill="url(#aero-dots)" mask="url(#aero-under)" opacity="0.38">
            {PUFFS.map(([x, y, r], i) => (
              <circle key={i} cx={x} cy={y} r={r} />
            ))}
          </g>
          <g fill="#ffffff" filter="url(#aero-glow)">
            {PUFFS.map(([x, y, r], i) => (
              <circle key={i} cx={x - 10} cy={y - 14} r={r * 0.62} />
            ))}
          </g>
        </g>
      </svg>
    </div>
  )
}

// Shared filters and patterns, defined once per document.
function Defs() {
  return (
    <svg width="0" height="0" className={s.defs} focusable="false">
      <defs>
        {[0, 1, 2].map((i) => (
          <filter key={i} id={`aero-billow-${i}`} x="-15%" y="-25%" width="130%" height="150%">
            <feTurbulence type="fractalNoise" baseFrequency={0.014 + i * 0.003} numOctaves="3" seed={i * 7 + 2} result="n" />
            <feDisplacementMap in="SourceGraphic" in2="n" scale={26 + i * 4} xChannelSelector="R" yChannelSelector="G" result="d" />
            <feGaussianBlur in="d" stdDeviation="1.1" />
          </filter>
        ))}
        <filter id="aero-soft" x="-20%" y="-20%" width="140%" height="140%">
          <feGaussianBlur stdDeviation="6" />
        </filter>
        <filter id="aero-glow" x="-20%" y="-20%" width="140%" height="140%">
          <feGaussianBlur stdDeviation="5" />
        </filter>
        <radialGradient id="aero-puff" cx="0.42" cy="0.34" r="0.7">
          <stop offset="0" stopColor="#ffffff" />
          <stop offset="0.5" stopColor="#fbfdff" />
          <stop offset="0.82" stopColor="#e2edf7" />
          <stop offset="1" stopColor="#c8dbee" />
        </radialGradient>
        <linearGradient id="aero-under-g" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0.45" stopColor="#000" />
          <stop offset="1" stopColor="#fff" />
        </linearGradient>
        <mask id="aero-under" maskContentUnits="objectBoundingBox">
          <rect width="1" height="1" fill="url(#aero-under-g)" />
        </mask>
        <filter id="aero-foliage" x="-5%" y="-30%" width="110%" height="160%">
          <feTurbulence type="fractalNoise" baseFrequency="0.05" numOctaves="2" seed="4" result="n" />
          <feDisplacementMap in="SourceGraphic" in2="n" scale="10" xChannelSelector="R" yChannelSelector="G" />
        </filter>
        <pattern id="aero-dots" width="6" height="6" patternUnits="userSpaceOnUse">
          <circle cx="3" cy="3" r="1.15" fill="#7d9fc0" />
        </pattern>
        <pattern id="aero-grass" width="14" height="10" patternUnits="userSpaceOnUse">
          <path d="M2 10 L3 3 M7 10 L6.5 2 M11 10 L12 4" stroke="#2f8a1f" strokeWidth="1" strokeLinecap="round" opacity="0.5" />
        </pattern>
        <linearGradient id="aero-sea" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#8fe0ef" />
          <stop offset="0.18" stopColor="#4cc3e0" />
          <stop offset="0.6" stopColor="#1e9fcf" />
          <stop offset="1" stopColor="#0c78ad" />
        </linearGradient>
        <linearGradient id="aero-meadow" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#b8ea5e" />
          <stop offset="0.35" stopColor="#7fcc3c" />
          <stop offset="1" stopColor="#3d9624" />
        </linearGradient>
        <linearGradient id="aero-hill" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#8fd04a" />
          <stop offset="1" stopColor="#4d9e2d" />
        </linearGradient>
        <radialGradient id="aero-sunlit" cx="0.3" cy="0.2" r="0.6">
          <stop offset="0" stopColor="#f4ffd0" stopOpacity="0.75" />
          <stop offset="1" stopColor="#f4ffd0" stopOpacity="0" />
        </radialGradient>
      </defs>
    </svg>
  )
}

// Static land: sea, far shore, treeline and the meadow. No animation inside,
// so its filters render once.
function Land() {
  return (
    <svg className={s.land} viewBox="0 0 1600 420" preserveAspectRatio="xMidYMax slice" focusable="false">
      <rect x="0" y="150" width="1600" height="270" fill="url(#aero-sea)" />
      {/* far shore, right */}
      <g filter="url(#aero-foliage)">
        <path d="M1180 152 C1260 132 1330 128 1400 134 C1470 126 1540 130 1600 124 L1600 156 L1180 156 Z" fill="#3f8a34" />
        <path d="M1230 154 C1300 142 1380 140 1460 146 C1520 140 1570 142 1600 140 L1600 158 L1230 158 Z" fill="#5aa83c" />
      </g>
      {/* treeline behind the meadow, left */}
      <g filter="url(#aero-foliage)">
        <path
          d="M0 96 C40 70 80 74 110 88 C140 62 190 66 215 86 C250 70 290 76 318 96 C350 80 392 86 420 104 C455 92 500 98 530 118 C560 110 600 118 640 140 L640 170 L0 170 Z"
          fill="#2f6e2a"
        />
        <path d="M0 120 C60 104 120 108 170 118 C230 104 300 110 360 124 C430 116 520 126 600 146 L600 172 L0 172 Z" fill="#3e8a32" />
      </g>
      {/* back hill */}
      <path d="M0 132 C180 112 380 128 560 150 C660 162 760 176 860 196 L860 420 L0 420 Z" fill="url(#aero-hill)" />
      {/* shallows where the meadow meets the sea */}
      <path d="M740 214 C860 238 960 290 1030 420" stroke="#a9eef5" strokeWidth="18" fill="none" opacity="0.7" />
      {/* front meadow */}
      <path d="M0 176 C240 146 500 166 720 212 C860 240 960 300 1020 420 L0 420 Z" fill="url(#aero-meadow)" />
      <path d="M0 176 C240 146 500 166 720 212 C860 240 960 300 1020 420 L0 420 Z" fill="url(#aero-grass)" opacity="0.55" />
      <path d="M0 176 C240 146 500 166 720 212 C860 240 960 300 1020 420 L0 420 Z" fill="url(#aero-sunlit)" />
      {/* right foreground bank */}
      <path d="M1600 300 C1500 310 1420 340 1380 420 L1600 420 Z" fill="url(#aero-meadow)" />
      <path d="M1600 300 C1500 310 1420 340 1380 420 L1600 420 Z" fill="url(#aero-grass)" opacity="0.5" />
    </svg>
  )
}

// Moving bits on the land, in their own layer without filters: water glints,
// the ferry boat, sparkles in the grass and swaying tufts.
function LandLife() {
  return (
    <svg className={s.land} viewBox="0 0 1600 420" preserveAspectRatio="xMidYMax slice" focusable="false">
      <g className={s.glints} stroke="#ffffff" strokeLinecap="round">
        <path d="M1080 190 h46 M1240 176 h30 M1380 204 h54 M1150 236 h38 M1310 250 h62 M1470 232 h34 M1210 300 h48 M1420 320 h40 M1110 350 h30" strokeWidth="2.4" opacity="0.7" />
      </g>
      <g className={s.glints2} stroke="#ffffff" strokeLinecap="round">
        <path d="M1120 214 h26 M1290 196 h40 M1440 270 h30 M1180 276 h36 M1350 290 h24 M1500 300 h44 M1260 340 h34" strokeWidth="1.8" opacity="0.55" />
      </g>
      <g className={s.boatPath}>
        <g className={s.boat}>
          {/* wake */}
          <path d="M-46 22 C-30 18 -14 20 0 22" stroke="#e6fbff" strokeWidth="3" fill="none" strokeLinecap="round" opacity="0.8" />
          {/* hull */}
          <path d="M-2 10 L58 10 L50 24 L6 24 Z" fill="#ffffff" stroke="#9fc4dc" strokeWidth="1" />
          <path d="M2 17 L55 17" stroke="#1e8fd6" strokeWidth="3" />
          {/* cabin */}
          <path d="M14 10 L14 0 Q14 -3 17 -3 L40 -3 Q43 -3 43 0 L43 10 Z" fill="#f4fbff" stroke="#9fc4dc" strokeWidth="1" />
          <rect x="18" y="1" width="6" height="5" rx="1.5" fill="#7cc8f2" />
          <rect x="27" y="1" width="6" height="5" rx="1.5" fill="#7cc8f2" />
          <rect x="36" y="1" width="4" height="5" rx="1.5" fill="#7cc8f2" />
          <path d="M24 -3 L24 -11 L32 -11 L32 -3" fill="#1e8fd6" />
        </g>
      </g>
      <g fill="#ffffff">
        {[
          [120, 300, 0],
          [260, 250, 1.2],
          [420, 330, 2.1],
          [560, 270, 0.6],
          [700, 360, 1.7],
          [330, 380, 2.8],
          [1480, 360, 1],
        ].map(([x, y, d], i) => (
          <path
            key={i}
            className={s.sparkle}
            style={{ animationDelay: `${d}s` }}
            transform={`translate(${x} ${y})`}
            d="M0 -7 C1 -1.5 1.5 -1 7 0 C1.5 1 1 1.5 0 7 C-1 1.5 -1.5 1 -7 0 C-1.5 -1 -1 -1.5 0 -7 Z"
          />
        ))}
      </g>
    </svg>
  )
}

function Tufts({ side }: { side: 'left' | 'right' }) {
  const blades = side === 'left' ? [6, 18, 30, 44, 58, 70, 86, 100, 116, 130, 148, 162] : [10, 24, 40, 52, 68, 84, 98, 112, 126]
  return (
    <svg className={`${s.tufts} ${side === 'left' ? s.tuftsL : s.tuftsR}`} viewBox="0 0 180 120" preserveAspectRatio="xMidYMax meet" focusable="false">
      {blades.map((x, i) => {
        const h = 60 + ((i * 37) % 50)
        const lean = ((i * 13) % 20) - 10
        return (
          <path
            key={i}
            className={s.blade}
            style={{ animationDelay: `${(i % 5) * 0.4}s`, animationDuration: `${4.5 + (i % 4) * 0.7}s` }}
            d={`M${x - 4} 120 Q${x + lean * 0.4} ${120 - h * 0.6} ${x + lean} ${120 - h} Q${x + lean * 0.2 + 2} ${120 - h * 0.5} ${x + 4} 120 Z`}
            fill={i % 3 === 0 ? '#9bdb4d' : i % 3 === 1 ? '#5db82e' : '#3f9a26'}
          />
        )
      })}
    </svg>
  )
}

function Gulls() {
  return (
    <div className={s.gulls}>
      {[0, 1, 2].map((i) => (
        <svg key={i} className={s.gull} style={{ animationDelay: `${i * 0.35}s` }} viewBox="0 0 20 8" focusable="false">
          <path d="M1 6 Q5 0 10 5 Q15 0 19 6" fill="none" stroke="#5d7f99" strokeWidth="1.4" strokeLinecap="round" />
        </svg>
      ))}
    </div>
  )
}

// Pointer parallax: writes --px/--py (-1..1) on the scene, throttled to one
// update per frame. CSS does the rest; React never re-renders for it.
export function usePointerParallax(ref: React.RefObject<HTMLElement | null>) {
  useEffect(() => {
    const el = ref.current
    if (!el || isReducedMotion() || !window.matchMedia('(hover: hover) and (pointer: fine)').matches) return
    let raf = 0
    let x = 0
    let y = 0
    const onMove = (e: PointerEvent) => {
      x = (e.clientX / window.innerWidth) * 2 - 1
      y = (e.clientY / window.innerHeight) * 2 - 1
      if (!raf)
        raf = requestAnimationFrame(() => {
          raf = 0
          el.style.setProperty('--px', x.toFixed(3))
          el.style.setProperty('--py', y.toFixed(3))
        })
    }
    window.addEventListener('pointermove', onMove, { passive: true })
    return () => {
      window.removeEventListener('pointermove', onMove)
      cancelAnimationFrame(raf)
    }
  }, [ref])
}

export function Scenery({ variant = 'full', className }: { variant?: 'full' | 'compact'; className?: string }) {
  const ref = useRef<HTMLDivElement>(null)
  usePointerParallax(ref)
  return (
    <div ref={ref} className={`${s.scene} ${variant === 'compact' ? s.compact : ''} ${className ?? ''}`} aria-hidden>
      <Defs />
      <div className={s.sky} />
      <div className={s.sun} />
      {variant === 'full' && <Gulls />}
      <div className={s.clouds}>
        {CLOUDS[variant].map((c) => (
          <Cloud key={c.cls} {...c} />
        ))}
      </div>
      <div className={s.landWrap}>
        <Land />
        <LandLife />
        {variant === 'full' && (
          <>
            <Tufts side="left" />
            <Tufts side="right" />
          </>
        )}
      </div>
      <div className={s.halftone} />
    </div>
  )
}
