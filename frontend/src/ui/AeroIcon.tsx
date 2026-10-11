import type { Icon } from '@phosphor-icons/react'
import type { CSSProperties } from 'react'
import s from './AeroIcon.module.css'

// Original glossy "Aero" icon: a gel tile or orb with a specular cap, a rim
// light and a soft tinted shadow, holding a Phosphor duotone glyph. We draw
// our own instead of using a downloaded pack (none is openly licensed, and the
// brief asks for original assets). Leaf is reserved for CO2.
export type AeroTone = 'sky' | 'aqua' | 'grass' | 'leaf'

export function AeroIcon({
  icon: Glyph,
  tone = 'sky',
  shape = 'tile',
  size = 56,
  label,
  float,
  className,
  style,
}: {
  icon: Icon
  tone?: AeroTone
  shape?: 'tile' | 'orb'
  size?: number
  label?: string
  float?: boolean
  className?: string
  style?: CSSProperties
}) {
  return (
    <span
      className={[s.icon, s[tone], shape === 'orb' && s.orb, float && s.float, className].filter(Boolean).join(' ')}
      style={{ '--s': `${size}px`, ...style } as CSSProperties}
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      <Glyph className={s.glyph} size={Math.round(size * 0.52)} weight="duotone" />
    </span>
  )
}
