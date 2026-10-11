import { X } from '@phosphor-icons/react'
import { useEffect, useId, useRef, type ReactNode } from 'react'
import s from './Sheet.module.css'

type Props = { open: boolean; onClose: () => void; title: ReactNode; subtitle?: ReactNode; children: ReactNode; footer?: ReactNode }

// Native <dialog>: focus trap, Escape and inert background for free.
export function Sheet({ open, onClose, title, subtitle, children, footer }: Props) {
  const ref = useRef<HTMLDialogElement>(null)
  const titleId = useId()
  useEffect(() => {
    const d = ref.current
    if (!d) return
    if (open && !d.open) d.showModal()
    if (!open && d.open) d.close()
  }, [open])

  return (
    <dialog
      ref={ref}
      className={s.sheet}
      aria-labelledby={titleId}
      onClose={onClose}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose()
      }}
    >
      {open && (
        <div className={s.body}>
          <div className={s.handle} aria-hidden />
          <div className={s.head}>
            <div>
              <h2 id={titleId}>{title}</h2>
              {subtitle && <p className="muted">{subtitle}</p>}
            </div>
            <button type="button" className={s.close} onClick={onClose} aria-label="Close">
              <X size={22} />
            </button>
          </div>
          <div className={s.content}>{children}</div>
          {footer && <div className={s.footer}>{footer}</div>}
        </div>
      )}
    </dialog>
  )
}
