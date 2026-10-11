import { CloudWarning, WifiSlash } from '@phosphor-icons/react'
import { useEffect, useState, type CSSProperties, type ReactNode } from 'react'
import { ApiError } from '../api/client'
import { Bubble } from './Bubble'
import { GelButton } from './GelButton'
import s from './States.module.css'

export function Skeleton({ h = 20, w = '100%', r, style }: { h?: number | string; w?: number | string; r?: number | string; style?: CSSProperties }) {
  return <div className={s.skeleton} style={{ height: h, width: w, borderRadius: r, ...style }} aria-hidden />
}

export function Loading({ label = 'Loading', rows = 3 }: { label?: string; rows?: number }) {
  return (
    <div className="stack" role="status" aria-live="polite">
      <span className="sr-only">{label}</span>
      {Array.from({ length: rows }, (_, i) => (
        <Skeleton key={i} h={i === 0 ? 72 : 56} />
      ))}
    </div>
  )
}

export function Empty({ icon, title, children, action }: { icon?: ReactNode; title: string; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className={s.state}>
      <Bubble size={64} clear>
        {icon}
      </Bubble>
      <h3>{title}</h3>
      {children && <p className="muted">{children}</p>}
      {action}
    </div>
  )
}

export function ErrorState({ error, onRetry, title = 'That didn’t load' }: { error: unknown; onRetry?: () => void; title?: string }) {
  const msg = error instanceof ApiError ? error.message : 'Something went wrong on our side. Try again in a moment.'
  return (
    <div className={s.state} role="alert">
      <CloudWarning size={40} className={s.errorIcon} aria-hidden />
      <h3>{title}</h3>
      <p className="muted">{msg}</p>
      {onRetry && (
        <GelButton variant="white" size="sm" onClick={onRetry}>
          Try again
        </GelButton>
      )}
    </div>
  )
}

export function OfflineBanner() {
  const [online, setOnline] = useState(() => navigator.onLine)
  useEffect(() => {
    const on = () => setOnline(true)
    const off = () => setOnline(false)
    window.addEventListener('online', on)
    window.addEventListener('offline', off)
    return () => {
      window.removeEventListener('online', on)
      window.removeEventListener('offline', off)
    }
  }, [])
  if (online) return null
  return (
    <div className={s.offline} role="status">
      <WifiSlash size={18} aria-hidden />
      You’re offline. Ferry will catch up when you reconnect.
    </div>
  )
}
