import { useId, type InputHTMLAttributes, type ReactNode } from 'react'
import s from './Field.module.css'

type Props = InputHTMLAttributes<HTMLInputElement> & { label: ReactNode; help?: ReactNode; error?: ReactNode }

export function Field({ label, help, error, className, ...rest }: Props) {
  const id = useId()
  const helpId = help ? `${id}-help` : undefined
  const errId = error ? `${id}-err` : undefined
  return (
    <div className={s.field}>
      <label htmlFor={id} className={s.label}>
        {label}
      </label>
      <input
        id={id}
        className={[rest.type === 'range' ? s.range : s.input, className].filter(Boolean).join(' ')}
        aria-describedby={[helpId, errId].filter(Boolean).join(' ') || undefined}
        aria-invalid={error ? true : undefined}
        {...rest}
      />
      {help && (
        <span id={helpId} className={s.help}>
          {help}
        </span>
      )}
      {error && (
        <span id={errId} className={s.error}>
          {error}
        </span>
      )}
    </div>
  )
}
