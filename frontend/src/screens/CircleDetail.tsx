import { CalendarPlus } from '@phosphor-icons/react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { ApiError } from '../api/client'
import { api } from '../api/endpoints'
import { upsertInvite, useCircle } from '../api/hooks'
import { ago, grams, plural } from '../lib/format'
import { Page, TopBar } from '../shell/TopBar'
import { Avatar, Bubble } from '../ui/Bubble'
import { Chip, chipGroupClass } from '../ui/Chip'
import { Field } from '../ui/Field'
import { GelButton } from '../ui/GelButton'
import { pop } from '../ui/pops'
import { Sheet } from '../ui/Sheet'
import { ErrorState, Loading } from '../ui/States'
import { Panel } from '../ui/Surface'
import c from './common.module.css'
import s from './Impact.module.css'

function when(kind: string) {
  const d = new Date()
  if (kind === 'tonight') d.setHours(19, 30, 0, 0)
  else if (kind === 'tomorrow') {
    d.setDate(d.getDate() + 1)
    d.setHours(19, 30, 0, 0)
  } else {
    d.setDate(d.getDate() + ((6 - d.getDay() + 7) % 7 || 7))
    d.setHours(12, 0, 0, 0)
  }
  return d.toISOString()
}

const TIMES = [
  { key: 'tonight', label: 'Tonight 7:30' },
  { key: 'tomorrow', label: 'Tomorrow 7:30' },
  { key: 'saturday', label: 'Saturday noon' },
  { key: 'none', label: 'Let them pick' },
]

export default function CircleDetail() {
  const { circleId = '' } = useParams()
  const circle = useCircle(circleId)
  const qc = useQueryClient()
  const nav = useNavigate()
  const [open, setOpen] = useState(false)
  const [msg, setMsg] = useState('')
  const [time, setTime] = useState('tomorrow')
  const meet = useMutation({
    mutationFn: () => api.meetAgain(circleId, { message: msg.trim() || undefined, proposed_time: time === 'none' ? undefined : when(time) }),
    onSuccess: (inv) => {
      upsertInvite(qc, inv)
      setOpen(false)
      pop(null, { label: 'Invite sent', sound: 'chime' })
      nav(`/i/${inv.id}`)
    },
  })

  useEffect(() => {
    if (circle.data) document.title = `${circle.data.name} on Ferry`
  }, [circle.data])

  return (
    <>
      <TopBar title={circle.data?.name ?? 'Circle'} back="/circles" />
      <Page>
        {circle.isPending ? (
          <Loading />
        ) : circle.isError ? (
          <Panel>
            <ErrorState error={circle.error} onRetry={() => circle.refetch()} />
          </Panel>
        ) : (
          <div className="stack">
            <Panel className={s.hero}>
              <Bubble size={72} clear seed={circle.data.id}>
                <span aria-hidden style={{ fontSize: 36 }}>
                  {circle.data.emoji}
                </span>
              </Bubble>
              <div className={s.heroText}>
                <h1 style={{ fontSize: 'var(--fs-xl)' }}>{circle.data.name}</h1>
                <p className="muted">
                  {circle.data.last_met_at ? `Last met ${ago(circle.data.last_met_at)}` : 'You haven’t met up yet'}. Active {ago(circle.data.last_active_at)}.
                </p>
              </div>
            </Panel>
            <div className={s.stats}>
              <Panel pad="tight" className={s.stat}>
                <span className={`${s.statNum} num`}>{circle.data.stats.runs}</span>
                <span className={s.statLabel}>ferry runs together</span>
              </Panel>
              <Panel pad="tight" className={s.stat}>
                <span className={`${s.statNum} num co2`}>{grams(circle.data.stats.co2_saved_g)}</span>
                <span className={s.statLabel}>CO2 saved together</span>
              </Panel>
            </div>
            <Panel>
              <h2 style={{ fontSize: 'var(--fs-md)', marginBottom: 12 }}>{plural(circle.data.members.length, 'member')}</h2>
              <ul className={c.list} style={{ gap: 8 }}>
                {circle.data.members.map((m) => (
                  <li key={m.id} style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
                    <Avatar user={m} size={36} label={false} />
                    <span>{m.display_name}</span>
                  </li>
                ))}
              </ul>
            </Panel>
            <GelButton size="lg" icon={<CalendarPlus size={22} aria-hidden />} onClick={() => setOpen(true)}>
              Meet again
            </GelButton>
          </div>
        )}
      </Page>
      <Sheet
        open={open}
        onClose={() => setOpen(false)}
        title="Meet again"
        subtitle={circle.data ? `Ferry invites everyone in ${circle.data.name} and starts a Telegram group when people say yes.` : undefined}
        footer={
          <GelButton block busy={meet.isPending} onClick={() => meet.mutate()}>
            Send invite
          </GelButton>
        }
      >
        <div className="stack">
          <Field label="Message (optional)" value={msg} maxLength={140} placeholder="Dinner at Lenoir again?" onChange={(e) => setMsg(e.target.value)} />
          <div className="stack" style={{ gap: 8 }}>
            <strong id="when-h">When</strong>
            <div className={chipGroupClass} role="radiogroup" aria-labelledby="when-h">
              {TIMES.map((t) => (
                <Chip key={t.key} role="radio" selected={time === t.key} onClick={() => setTime(t.key)}>
                  {t.label}
                </Chip>
              ))}
            </div>
          </div>
          {meet.isError && (
            <p className={c.error} role="alert">
              {meet.error instanceof ApiError ? meet.error.message : 'The invite didn’t send. Try again.'}
            </p>
          )}
        </div>
      </Sheet>
    </>
  )
}
