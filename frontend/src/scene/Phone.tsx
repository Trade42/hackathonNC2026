import type { ReactNode } from 'react'
import s from './Phone.module.css'

// A generic modern phone drawn in CSS: titanium edge, thin black bezel,
// punch-hole camera and a glass glare. No brand marks.
export function Phone({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div className={`${s.stage} ${className ?? ''}`}>
      <div className={s.rise}>
        <div className={s.float}>
          <div className={s.phone}>
            <span className={s.btnPower} />
            <span className={s.btnVolUp} />
            <span className={s.btnVolDown} />
            <div className={s.bezel}>
              <div className={s.screen}>
                {children}
                <span className={s.camera} />
                <span className={s.glare} />
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  )
}
