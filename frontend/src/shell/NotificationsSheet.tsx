import { Bell } from '@phosphor-icons/react'
import { useQueryClient } from '@tanstack/react-query'
import { useNavigate } from 'react-router-dom'
import { api } from '../api/endpoints'
import { keys, useNotifications } from '../api/hooks'
import type { Notification } from '../api/types'
import { ago } from '../lib/format'
import { Sheet } from '../ui/Sheet'
import { Empty, ErrorState, Loading } from '../ui/States'
import { notificationHref } from './links'
import s from './Shell.module.css'

export function NotificationsSheet({ open, onClose }: { open: boolean; onClose: () => void }) {
  const q = useNotifications()
  const qc = useQueryClient()
  const nav = useNavigate()

  const openOne = (n: Notification) => {
    if (!n.read) {
      qc.setQueryData<Notification[]>(keys.notifications, (p) => p?.map((x) => (x.id === n.id ? { ...x, read: true } : x)))
      api.readNotification(n.id).catch(() => {})
    }
    const href = notificationHref(n)
    onClose()
    if (href) nav(href)
  }

  return (
    <Sheet open={open} onClose={onClose} title="Notifications">
      {q.isPending ? (
        <Loading label="Loading notifications" />
      ) : q.isError ? (
        <ErrorState error={q.error} onRetry={() => q.refetch()} />
      ) : !q.data.length ? (
        <Empty icon={<Bell size={28} color="var(--sky-700)" />} title="All caught up">
          When someone opens a ferry or invites you, it shows up here.
        </Empty>
      ) : (
        <ul className={s.ntfList}>
          {q.data.map((n) => (
            <li key={n.id}>
              <button type="button" className={s.ntf} data-unread={!n.read} onClick={() => openOne(n)}>
                <span style={{ flex: 1, display: 'grid', gap: 2 }}>
                  <strong style={{ fontWeight: 600 }}>{n.title}</strong>
                  <span className="muted">{n.body}</span>
                  <time dateTime={n.created_at}>{ago(n.created_at)}</time>
                </span>
                {!n.read && <span className="sr-only">Unread</span>}
              </button>
            </li>
          ))}
        </ul>
      )}
    </Sheet>
  )
}
