import { X } from '@phosphor-icons/react'
import { AnimatePresence, motion } from 'motion/react'
import { useEffect, useRef, useState } from 'react'
import { useLocation, useNavigate } from 'react-router-dom'
import { realtime } from '../api/realtime'
import type { Notification } from '../api/types'
import { Bubble } from '../ui/Bubble'
import { sound } from '../lib/sound'
import { notificationHref } from './links'
import s from './Shell.module.css'

// In-app banner for notification.created events. One at a time, 6 s each.
export function NotificationBanner() {
  const [current, setCurrent] = useState<Notification | null>(null)
  const timer = useRef<number | undefined>(undefined)
  const nav = useNavigate()
  const loc = useLocation()
  const path = useRef(loc.pathname)
  path.current = loc.pathname

  useEffect(() => {
    const off = realtime.onEvent((e) => {
        if (e.type !== 'notification.created') return
        const n = e.data.notification
        // Don't announce what the person is already looking at.
        if (notificationHref(n) === path.current) return
        setCurrent(n)
        sound.drop()
        window.clearTimeout(timer.current)
        timer.current = window.setTimeout(() => setCurrent(null), 6000)
    })
    return () => {
      off()
    }
  }, [])

  return (
    <div className={s.bannerWrap} aria-live="polite">
      <AnimatePresence>
        {current && (
          <motion.div
            key={current.id}
            className={s.banner}
            initial={{ y: -24, opacity: 0 }}
            animate={{ y: 0, opacity: 1 }}
            exit={{ y: -16, opacity: 0 }}
            transition={{ type: 'spring', stiffness: 520, damping: 34 }}
          >
            <button
              type="button"
              className={s.bannerOpen}
              onClick={() => {
                const href = notificationHref(current)
                setCurrent(null)
                if (href) nav(href)
              }}
            >
              <Bubble size={36} color={current.type === 'invite' ? '#8A5CD0' : '#1E8FD6'} seed={current.id} />
              <span className={s.bannerText}>
                <strong>{current.title}</strong>
                <span>{current.body}</span>
              </span>
            </button>
            <button type="button" className={s.iconBtn} aria-label="Dismiss" onClick={() => setCurrent(null)}>
              <X size={18} aria-hidden />
            </button>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  )
}
