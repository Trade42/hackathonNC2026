import { Check } from '@phosphor-icons/react'
import type { ButtonHTMLAttributes, ReactNode } from 'react'
import s from './Chip.module.css'

export const chipGroupClass = s.group

type Props = ButtonHTMLAttributes<HTMLButtonElement> & { selected?: boolean; role?: 'radio' | 'checkbox'; children: ReactNode }

// A toggle chip. Pass role="radio" inside a radiogroup for single choice.
export function Chip({ selected = false, role, children, className, ...rest }: Props) {
  const aria = role ? { role, 'aria-checked': selected } : { 'aria-pressed': selected }
  return (
    <button type="button" className={[s.chip, className].filter(Boolean).join(' ')} {...aria} {...rest}>
      {selected && (
        <span className={s.check} aria-hidden>
          <Check size={12} weight="bold" />
        </span>
      )}
      {children}
    </button>
  )
}
