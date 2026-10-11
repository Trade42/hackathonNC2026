import { ArrowLeft, Bell, GearSix } from '@phosphor-icons/react'
import { useState, type ReactNode } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useNotifications } from '../api/hooks'
import { glassClass } from '../ui/Surface'
import { SoundToggle } from '../ui/SoundToggle'
import { Logo } from './Logo'
import { NotificationsSheet } from './NotificationsSheet'
import s from './Shell.module.css'

type Props = { title?: string; back?: string | true; actions?: ReactNode; brand?: boolean }

export function TopBar({ title, back, actions, brand }: Props) {
  const nav = useNavigate()
  const { data: ntfs } = useNotifications()
  const [open, setOpen] = useState(false)
  const unread = ntfs?.filter((n) => !n.read).length ?? 0
  return (
    <header className={`${s.top} ${glassClass}`}>
      {back && (
        <button
          type="button"
          className={s.iconBtn}
          aria-label="Back"
          onClick={() => (back === true ? (window.history.length > 1 ? nav(-1) : nav('/home')) : nav(back))}
        >
          <ArrowLeft size={22} aria-hidden />
        </button>
      )}
      {brand ? (
        <div className={s.topBrand}>
          <span className={s.logoSmall}>
            <Logo size={26} />
          </span>
          {title && <h1 className="sr-only">{title}</h1>}
        </div>
      ) : (
        <h1 style={back ? undefined : { paddingLeft: 8 }}>{title}</h1>
      )}
      <div className={s.topActions}>
        {actions}
        <SoundToggle className={s.iconBtn} />
        <button type="button" className={s.iconBtn} aria-label={unread ? `Notifications, ${unread} unread` : 'Notifications'} onClick={() => setOpen(true)}>
          <Bell size={22} aria-hidden />
          {unread > 0 && (
            <span className={s.badge} aria-hidden>
              {unread}
            </span>
          )}
        </button>
        <Link to="/settings" className={s.iconBtn} aria-label="Settings">
          <GearSix size={22} aria-hidden />
        </Link>
      </div>
      <NotificationsSheet open={open} onClose={() => setOpen(false)} />
    </header>
  )
}

export function Page({ children, wide }: { children: ReactNode; wide?: boolean }) {
  return <div className={`${s.page} ${wide ? s.pageWide : ''}`}>{children}</div>
}
