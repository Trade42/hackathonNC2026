import { useId, type CSSProperties } from 'react'
import s from './RunnerMark.module.css'

// The Ferry mark: a person sprinting inside the glossy Ferry bubble with a
// takeout bag, because a real friend carries the food (Ferry only organizes
// it). Drawn from scratch. "loop" runs forever, "once" runs three strides and
// again on hover, false holds a mid-stride pose. Reduced motion always holds.
export function RunnerMark({
  size = 32,
  animate = false,
  label,
  className,
  style,
}: {
  size?: number
  animate?: 'loop' | 'once' | false
  label?: string
  className?: string
  style?: CSSProperties
}) {
  const id = useId().replace(/:/g, '')
  const mode = animate === 'loop' ? s.loop : animate === 'once' ? s.once : s.still
  return (
    <svg
      viewBox="0 0 100 100"
      width={size}
      height={size}
      className={[s.mark, mode, className].filter(Boolean).join(' ')}
      style={style}
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      focusable="false"
    >
      <defs>
        <linearGradient id={`${id}o`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#8fd6ff" />
          <stop offset=".55" stopColor="#1e8fd6" />
          <stop offset="1" stopColor="#0a5e93" />
        </linearGradient>
        <linearGradient id={`${id}c`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#fff" stopOpacity=".9" />
          <stop offset="1" stopColor="#fff" stopOpacity=".05" />
        </linearGradient>
      </defs>
      <circle cx="50" cy="50" r="47" fill={`url(#${id}o)`} stroke="#fff" strokeWidth="2.5" />
      <ellipse cx="38" cy="22" rx="20" ry="9" fill={`url(#${id}c)`} transform="rotate(-18 38 22)" />
      <g className={s.speed} stroke="#fff" strokeWidth="3" strokeLinecap="round" opacity=".7">
        <line x1="17" y1="44" x2="27" y2="44" />
        <line x1="13" y1="54" x2="25" y2="54" />
        <line x1="17" y1="64" x2="26" y2="64" />
      </g>
      <g className={s.bob} stroke="#fff" strokeWidth="5.5" strokeLinecap="round" fill="none">
        <circle cx="58" cy="22" r="7" fill="#fff" stroke="none" />
        <line x1="55" y1="31" x2="48" y2="57" />
        <g className={s.legA}>
          <line x1="48" y1="57" x2="46" y2="72" />
          <g className={s.shinA}>
            <line x1="46" y1="72" x2="44" y2="88" />
          </g>
        </g>
        <g className={s.legB}>
          <line x1="48" y1="57" x2="46" y2="72" />
          <g className={s.shinB}>
            <line x1="46" y1="72" x2="44" y2="88" />
          </g>
        </g>
        <g className={s.armA}>
          <line x1="54" y1="37" x2="50" y2="47" />
          <line x1="50" y1="47" x2="58" y2="53" />
        </g>
        <g className={s.armB}>
          <line x1="54" y1="37" x2="50" y2="47" />
          <line x1="50" y1="47" x2="58" y2="53" />
          <path d="M57 52 h9 l-1 10 h-7 z" fill="#ffd89a" stroke="none" />
          <path d="M59.5 52 q2.5 -4 5 0" stroke="#ffd89a" strokeWidth="1.6" />
        </g>
      </g>
    </svg>
  )
}
