import { forwardRef, type ButtonHTMLAttributes, type ReactNode } from 'react'
import { Link, type LinkProps } from 'react-router-dom'
import s from './Gel.module.css'

type Variant = 'sky' | 'white' | 'eco' | 'danger'
type Size = 'sm' | 'md' | 'lg'

type Common = { variant?: Variant; size?: Size; block?: boolean; busy?: boolean; icon?: ReactNode; className?: string; children?: ReactNode }

export const gelClass = ({ variant = 'sky', size = 'md', block, busy, className }: Omit<Common, 'icon' | 'children'>) =>
  [s.gel, variant !== 'sky' && s[variant], size !== 'md' && s[size], block && s.block, busy && s.busy, className].filter(Boolean).join(' ')

export const GelButton = forwardRef<HTMLButtonElement, Common & ButtonHTMLAttributes<HTMLButtonElement>>(function GelButton(
  { variant, size, block, busy, icon, className, children, type = 'button', disabled, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      className={gelClass({ variant, size, block, busy, className })}
      disabled={disabled}
      aria-busy={busy || undefined}
      {...rest}
    >
      {busy ? <span className={s.spinner} aria-hidden /> : icon}
      {children}
    </button>
  )
})

export function GelLink({ variant, size, block, icon, className, children, ...rest }: Common & LinkProps) {
  return (
    <Link className={gelClass({ variant, size, block, className })} {...rest}>
      {icon}
      {children}
    </Link>
  )
}

export function GelAnchor({ variant, size, block, icon, className, children, ...rest }: Common & React.AnchorHTMLAttributes<HTMLAnchorElement>) {
  return (
    <a className={gelClass({ variant, size, block, className })} {...rest}>
      {icon}
      {children}
    </a>
  )
}
