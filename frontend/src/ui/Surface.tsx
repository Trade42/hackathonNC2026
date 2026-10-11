import type { HTMLAttributes } from 'react'
import s from './Surface.module.css'

export const glassClass = s.glass

type PanelProps = HTMLAttributes<HTMLDivElement> & { pad?: 'normal' | 'tight' | 'flush'; as?: 'div' | 'section' | 'article' | 'li' }

export function Panel({ pad = 'normal', as: Tag = 'div', className, ...rest }: PanelProps) {
  const cls = [s.panel, pad === 'tight' && s.tight, pad === 'flush' && s.flush, className].filter(Boolean).join(' ')
  const El = Tag as 'div'
  return <El className={cls} {...rest} />
}
