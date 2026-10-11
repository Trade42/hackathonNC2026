import type { CSSProperties } from 'react'
import s from './CuisineArt.module.css'

// Original food illustrations for restaurants without a photo (every fixture
// restaurant today). One per cuisine family, on a warm gradient with a few
// slow Aero bubbles. Decorative: the restaurant name is always beside it.

export type Family = 'taco' | 'burger' | 'bowl' | 'pita' | 'cookie' | 'bagel' | 'basket' | 'plate'

const MATCH: [Family, RegExp][] = [
  ['taco', /mexican|taco|burrito|tex/i],
  ['burger', /burger|hot dog|american|grill/i],
  ['bowl', /vietnam|pho|thai|asian|ramen|noodle|korean|japanese|chinese/i],
  ['pita', /mediterr|middle eastern|greek|falafel|shawarma/i],
  ['cookie', /dessert|cookie|bakery|ice cream|sweet/i],
  ['bagel', /bagel|breakfast|coffee|cafe/i],
  ['basket', /grocery|deli|market|hot bar/i],
]

export function cuisineFamily(cuisine: string[] | undefined): Family {
  const all = (cuisine ?? []).join(' ')
  return MATCH.find(([, re]) => re.test(all))?.[0] ?? 'plate'
}

function Food({ family }: { family: Family }) {
  switch (family) {
    case 'taco':
      return (
        <g>
          <path d="M14 66 A36 36 0 0 1 86 66 Z" fill="#f4b740" stroke="#fff" strokeWidth="3" />
          <path d="M20 60 C 28 48, 36 54, 42 46 S 58 50, 64 44 S 76 52, 80 58" fill="none" stroke="#6cc04a" strokeWidth="7" strokeLinecap="round" />
          <circle cx="36" cy="52" r="4" fill="#e8553f" />
          <circle cx="56" cy="47" r="4" fill="#e8553f" />
          <circle cx="68" cy="53" r="3.5" fill="#e8553f" />
          <path d="M22 64 A28 28 0 0 1 78 64" fill="none" stroke="#e39b22" strokeWidth="2" opacity=".6" />
        </g>
      )
    case 'burger':
      return (
        <g stroke="#fff" strokeWidth="2.5">
          <path d="M18 46 C 18 26, 82 26, 82 46 Z" fill="#f0a64a" />
          <path d="M16 50 Q 26 56 36 50 T 56 50 T 76 50 T 86 50" fill="none" stroke="#6cc04a" strokeWidth="6" strokeLinecap="round" />
          <rect x="17" y="54" width="66" height="10" rx="5" fill="#7a4526" />
          <path d="M18 56 l12 8 l10 -8 l12 8 l10 -8 l12 8 l8 -6" fill="#ffd23f" stroke="none" />
          <path d="M18 66 H82 V70 Q82 78 74 78 H26 Q18 78 18 70 Z" fill="#e8963a" />
          <circle cx="38" cy="36" r="1.8" fill="#fff6d8" stroke="none" />
          <circle cx="50" cy="32" r="1.8" fill="#fff6d8" stroke="none" />
          <circle cx="62" cy="36" r="1.8" fill="#fff6d8" stroke="none" />
        </g>
      )
    case 'bowl':
      return (
        <g>
          <path d="M60 18 L84 44" stroke="#c98a4a" strokeWidth="4" strokeLinecap="round" />
          <path d="M68 14 L88 40" stroke="#b5773a" strokeWidth="4" strokeLinecap="round" />
          <path d="M22 50 C 30 40, 70 40, 78 50" fill="none" stroke="#f6e3b4" strokeWidth="6" strokeLinecap="round" />
          <path d="M26 48 C 36 38, 64 40, 74 46" fill="none" stroke="#fff0c9" strokeWidth="4" strokeLinecap="round" />
          <circle cx="40" cy="45" r="5" fill="#6cc04a" />
          <circle cx="58" cy="44" r="5" fill="#f08a5d" />
          <path d="M14 52 H86 C 86 70, 70 82, 50 82 C 30 82, 14 70, 14 52 Z" fill="#ffffff" stroke="#9fd0ee" strokeWidth="3" />
          <path d="M20 58 H80" stroke="#1e8fd6" strokeWidth="3" />
        </g>
      )
    case 'pita':
      return (
        <g>
          <path d="M18 70 A32 32 0 0 1 82 70 Z" fill="#f2cf8a" stroke="#fff" strokeWidth="3" />
          <path d="M26 62 C 34 50, 44 56, 50 48 S 64 54, 74 60" fill="none" stroke="#6cc04a" strokeWidth="6" strokeLinecap="round" />
          <circle cx="40" cy="56" r="5" fill="#b46a3c" />
          <circle cx="58" cy="54" r="5" fill="#b46a3c" />
          <circle cx="50" cy="60" r="3.5" fill="#e8553f" />
        </g>
      )
    case 'cookie':
      return (
        <g>
          <circle cx="50" cy="50" r="30" fill="#d9a35f" stroke="#fff" strokeWidth="3" />
          <circle cx="50" cy="50" r="25" fill="none" stroke="#c78a45" strokeWidth="2" opacity=".5" />
          {[
            [40, 40],
            [58, 44],
            [46, 58],
            [62, 60],
            [36, 54],
          ].map(([x, y], i) => (
            <ellipse key={i} cx={x} cy={y} rx="3.6" ry="3" fill="#5a3418" />
          ))}
        </g>
      )
    case 'bagel':
      return (
        <g>
          <circle cx="50" cy="52" r="28" fill="#e8a75a" stroke="#fff" strokeWidth="3" />
          <circle cx="50" cy="52" r="9" fill="#fff7e6" stroke="#c98a45" strokeWidth="2" />
          {[
            [36, 40],
            [62, 38],
            [70, 56],
            [32, 60],
            [50, 30],
            [56, 72],
          ].map(([x, y], i) => (
            <ellipse key={i} cx={x} cy={y} rx="2.2" ry="1.3" fill="#fff6d8" transform={`rotate(${i * 30} ${x} ${y})`} />
          ))}
        </g>
      )
    case 'basket':
      return (
        <g>
          <path d="M60 22 l10 -8" stroke="#8a5a2b" strokeWidth="3" strokeLinecap="round" />
          <rect x="44" y="18" width="12" height="40" rx="6" fill="#e9b765" transform="rotate(-24 50 38)" />
          <circle cx="62" cy="40" r="10" fill="#e8553f" />
          <path d="M60 30 q2 -5 6 -6" stroke="#4f9a2e" strokeWidth="3" fill="none" />
          <path d="M16 46 H84 L76 80 H24 Z" fill="#1e8fd6" stroke="#fff" strokeWidth="3" strokeLinejoin="round" />
          <path d="M30 54 V74 M42 54 V74 M54 54 V74 M66 54 V74" stroke="#7cc8f2" strokeWidth="3" />
        </g>
      )
    default:
      return (
        <g>
          <circle cx="50" cy="52" r="30" fill="#ffffff" stroke="#9fd0ee" strokeWidth="3" />
          <circle cx="50" cy="52" r="20" fill="#eaf7fe" />
          <path d="M14 30 V74 M10 30 V42 M18 30 V42" stroke="#7aa6c4" strokeWidth="3" strokeLinecap="round" />
          <path d="M86 30 C 80 36, 80 46, 86 50 V74" stroke="#7aa6c4" strokeWidth="3" strokeLinecap="round" fill="none" />
        </g>
      )
  }
}

export function CuisineArt({
  cuisine,
  variant = 'thumb',
  className,
  style,
}: {
  cuisine: string[] | undefined
  variant?: 'thumb' | 'card' | 'banner'
  className?: string
  style?: CSSProperties
}) {
  const family = cuisineFamily(cuisine)
  return (
    <span className={[s.art, s[family], s[variant], className].filter(Boolean).join(' ')} style={style} aria-hidden>
      <span className={s.bubble} />
      <span className={`${s.bubble} ${s.b2}`} />
      <svg viewBox="0 0 100 100" className={s.food} focusable="false">
        <Food family={family} />
      </svg>
    </span>
  )
}
