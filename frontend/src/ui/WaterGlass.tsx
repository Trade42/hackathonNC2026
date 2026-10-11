import { useReducedMotion } from '../lib/prefs'

// Onboarding progress: the glass fills as questions get answered.
export function WaterGlass({ progress, size = 56 }: { progress: number; size?: number }) {
  const reduced = useReducedMotion()
  const p = Math.max(0, Math.min(1, progress))
  const h = size * 1.4
  // Water travels from the bottom of the glass (y=100) to near the rim (y=14).
  const y = 100 - p * 86
  return (
    <svg width={size} height={h} viewBox="0 0 70 110" aria-hidden style={{ overflow: 'visible' }}>
      <defs>
        <clipPath id="wg-clip">
          <path d="M8 6 L62 6 L56 102 Q55 106 51 106 L19 106 Q15 106 14 102 Z" />
        </clipPath>
        <linearGradient id="wg-water" x1="0" x2="0" y1="0" y2="1">
          <stop offset="0" stopColor="#7FDCEB" />
          <stop offset="1" stopColor="#1E9CC9" />
        </linearGradient>
        <linearGradient id="wg-glass" x1="0" x2="1">
          <stop offset="0" stopColor="#fff" stopOpacity=".85" />
          <stop offset=".35" stopColor="#fff" stopOpacity=".15" />
          <stop offset="1" stopColor="#fff" stopOpacity=".5" />
        </linearGradient>
      </defs>
      <path d="M8 6 L62 6 L56 102 Q55 106 51 106 L19 106 Q15 106 14 102 Z" fill="rgba(234,247,254,.6)" />
      <g clipPath="url(#wg-clip)">
        <g style={{ transform: `translateY(${y}px)`, transition: reduced ? 'none' : 'transform 350ms cubic-bezier(.34,1.56,.64,1)' }}>
          <path d="M-10 4 Q 8 -2 26 4 T 62 4 T 98 4 L 98 120 L -10 120 Z" fill="url(#wg-water)" />
          <path d="M-10 4 Q 8 -2 26 4 T 62 4 T 98 4" fill="none" stroke="#fff" strokeOpacity=".8" strokeWidth="1.5" />
        </g>
      </g>
      <path d="M8 6 L62 6 L56 102 Q55 106 51 106 L19 106 Q15 106 14 102 Z" fill="url(#wg-glass)" stroke="#5FA8D3" strokeWidth="2" />
      <path d="M15 14 L19 90" stroke="#fff" strokeWidth="3" strokeLinecap="round" strokeOpacity=".9" />
    </svg>
  )
}
