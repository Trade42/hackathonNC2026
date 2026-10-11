import { useId, type ReactNode } from 'react'
import s from './Switch.module.css'

export function SwitchRow({ label, help, checked, onChange, disabled }: { label: ReactNode; help?: ReactNode; checked: boolean; onChange: (v: boolean) => void; disabled?: boolean }) {
  const id = useId()
  return (
    <div className={s.row}>
      <span className={s.text}>
        <strong id={`${id}-l`}>{label}</strong>
        {help && <span id={`${id}-h`}>{help}</span>}
      </span>
      <button
        type="button"
        role="switch"
        className={s.switch}
        aria-checked={checked}
        aria-labelledby={`${id}-l`}
        aria-describedby={help ? `${id}-h` : undefined}
        disabled={disabled}
        onClick={() => onChange(!checked)}
      >
        <span className={s.knob} aria-hidden />
      </button>
    </div>
  )
}
