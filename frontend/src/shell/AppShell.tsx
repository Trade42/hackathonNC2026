import { House, MagnifyingGlass, Receipt, UserCircle, UsersThree } from '@phosphor-icons/react'
import { MotionConfig, motion } from 'motion/react'
import { useRef } from 'react'
import { NavLink, useLocation, useOutlet } from 'react-router-dom'
import { useReducedMotion } from '../lib/prefs'
import { Scenery } from '../scene/Scenery'
import { glassClass } from '../ui/Surface'
import { SoundToggle } from '../ui/SoundToggle'
import { Logo } from './Logo'
import { MiniTracker } from './MiniTracker'
import { NotificationBanner } from './NotificationBanner'
import s from './Shell.module.css'

// The You tab also covers the account screens it links to.
const tabs = [
  { to: '/home', label: 'Home', Icon: House, also: [] as string[] },
  { to: '/search', label: 'Search', Icon: MagnifyingGlass, also: [] },
  { to: '/orders', label: 'Orders', Icon: Receipt, also: [] },
  { to: '/circles', label: 'Circles', Icon: UsersThree, also: [] },
  { to: '/you', label: 'You', Icon: UserCircle, also: ['/wallet', '/impact', '/settings'] },
]

const TAB_PATHS = new Set(tabs.map((t) => t.to))
const depth = (path: string) => (TAB_PATHS.has(path) ? 0 : path.split('/').filter(Boolean).length)

// Screens slide in like an iOS push when you go deeper, slide back when you
// return, and rise softly when you switch tabs. CSS only, keyed by path.
function useTransition(pathname: string) {
  const prev = useRef(pathname)
  const dir = useRef<'push' | 'pop' | 'tab'>('tab')
  if (prev.current !== pathname) {
    const a = depth(prev.current)
    const b = depth(pathname)
    dir.current = b > a ? 'push' : b < a ? 'pop' : 'tab'
    prev.current = pathname
  }
  return dir.current
}

export default function AppShell() {
  const reduced = useReducedMotion()
  const { pathname } = useLocation()
  const outlet = useOutlet()
  const dir = useTransition(pathname)
  return (
    <MotionConfig reducedMotion={reduced ? 'always' : 'never'}>
      <div className={s.shell}>
        <Scenery variant="compact" />
        <a className="skip-link" href="#main">
          Skip to content
        </a>
        <main id="main" className={s.main} tabIndex={-1}>
          <div key={pathname} className={`${s.screen} ${dir === 'push' ? s.screenPush : dir === 'pop' ? s.screenPop : ''}`}>
            {outlet}
          </div>
        </main>
        <nav className={`${s.nav} ${glassClass}`} aria-label="Main">
          <div className={s.brand}>
            <NavLink to="/home" aria-label="Ferry home">
              <Logo size={34} word={false} />
            </NavLink>
          </div>
          {tabs.map(({ to, label, Icon, also }) => {
            const active = pathname === to || also.includes(pathname)
            return (
              <NavLink key={to} to={to} className={s.tab} data-active={active}>
                {active && <motion.span layoutId="tab-pill" className={s.tabPill} transition={{ type: 'spring', stiffness: 520, damping: 38 }} />}
                <span className={s.tabIcon}>
                  <Icon size={22} weight={active ? 'fill' : 'regular'} aria-hidden />
                </span>
                <span className={s.tabLabel}>{label}</span>
              </NavLink>
            )
          })}
          <div className={s.railExtras}>
            <SoundToggle className={s.iconBtn} />
          </div>
        </nav>
        <MiniTracker />
        <NotificationBanner />
      </div>
    </MotionConfig>
  )
}
