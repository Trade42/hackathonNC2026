import type { CSSProperties, ReactNode } from 'react'
import { useState } from 'react'
import { BowlSteam, Coffee, Cookie, ForkKnife, Hamburger, Leaf, Pepper, Storefront } from '@phosphor-icons/react'
import type { PublicUser } from '../api/types'
import s from './Bubble.module.css'

type BubbleProps = {
  size?: number
  color?: string
  clear?: boolean
  seed?: string
  className?: string
  style?: CSSProperties
  children?: ReactNode
  label?: string
}

// Highlight position derived from a seed, so each person's bubble catches the
// light a little differently but always the same way.
function highlight(seed = '') {
  let h = 0
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) | 0
  return { hx: `${26 + (Math.abs(h) % 14)}%`, hy: `${22 + (Math.abs(h >> 4) % 12)}%` }
}

export function Bubble({ size = 44, color, clear, seed, className, style, children, label }: BubbleProps) {
  const { hx, hy } = highlight(seed)
  return (
    <span
      className={[s.bubble, clear && s.clear, className].filter(Boolean).join(' ')}
      style={{ '--size': `${size}px`, '--hx': hx, '--hy': hy, ...(color ? { '--c': color } : {}), ...style } as CSSProperties}
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      {children}
    </span>
  )
}

export function Avatar({ user, size = 36, label = true }: { user: Pick<PublicUser, 'display_name' | 'bubble_color' | 'avatar_seed'>; size?: number; label?: boolean }) {
  const initial = (user.display_name || '?').trim().charAt(0).toUpperCase()
  return (
    <Bubble size={size} color={user.bubble_color} seed={user.avatar_seed} label={label ? user.display_name || 'You' : undefined}>
      <span className={s.initial} aria-hidden>
        {initial}
      </span>
    </Bubble>
  )
}

export function AvatarStack({ users, size = 30, max = 4 }: { users: PublicUser[]; size?: number; max?: number }) {
  const shown = users.slice(0, max)
  return (
    <span className={s.stack} role="img" aria-label={users.map((u) => u.display_name).join(', ')}>
      {shown.map((u) => (
        <Avatar key={u.id} user={u} size={size} label={false} />
      ))}
    </span>
  )
}

const cuisineIcon = (cuisine: string[] = []) => {
  const c = cuisine.join(' ').toLowerCase()
  if (/mexic|burrito|taco/.test(c)) return Pepper
  if (/burger|hot dog|american/.test(c)) return Hamburger
  if (/cookie|dessert|bak/.test(c)) return Cookie
  if (/coffee|bagel|cafe|breakfast/.test(c)) return Coffee
  if (/pho|vietnam|noodle|thai|ramen/.test(c)) return BowlSteam
  if (/grocery|salad|hot bar|vegan/.test(c)) return Leaf
  if (/mediterr|falafel|greek|middle eastern/.test(c)) return ForkKnife
  return Storefront
}

// Photo when there is one; the cuisine bubble when it's null or fails to load.
export function FoodBubble({ cuisine, imageUrl, size = 56, name }: { cuisine?: string[]; imageUrl?: string | null; size?: number; name: string }) {
  const [state, setState] = useState<'loading' | 'loaded' | 'failed'>('loading')
  const Icon = cuisineIcon(cuisine)
  const showImg = imageUrl && state !== 'failed'
  return (
    <Bubble size={size} clear seed={name}>
      {(!showImg || state !== 'loaded') && <Icon size={Math.round(size * 0.46)} weight="duotone" color="var(--sky-700)" aria-hidden />}
      {showImg && (
        <img
          className={s.img}
          src={imageUrl}
          alt=""
          loading="lazy"
          decoding="async"
          data-loaded={state === 'loaded'}
          onLoad={() => setState('loaded')}
          onError={() => setState('failed')}
        />
      )}
    </Bubble>
  )
}
