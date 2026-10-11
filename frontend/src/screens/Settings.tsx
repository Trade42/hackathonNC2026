import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { ApiError } from '../api/client'
import { api } from '../api/endpoints'
import { keys, useConfig, useMe } from '../api/hooks'
import { realtime } from '../api/realtime'
import type { Profile } from '../api/types'
import { env } from '../config/env'
import { money } from '../lib/format'
import { setPrefs, usePrefs } from '../lib/prefs'
import { enablePush } from '../lib/push'
import { sound } from '../lib/sound'
import { Page, TopBar } from '../shell/TopBar'
import { session } from '../state/session'
import { Avatar } from '../ui/Bubble'
import { Chip, chipGroupClass } from '../ui/Chip'
import { GelButton, GelLink } from '../ui/GelButton'
import { ErrorState, Loading } from '../ui/States'
import { Panel } from '../ui/Surface'
import { SwitchRow } from '../ui/Switch'
import c from './common.module.css'

function DemoControls() {
  const [msg, setMsg] = useState('')
  const run = async (fn: 'reset' | 'skip' | 'invite') => {
    const db = await import('../mocks/db')
    if (fn === 'reset') {
      db.reset()
      setMsg('Demo reset. Reloading.')
      setTimeout(() => window.location.assign('/home'), 400)
    } else if (fn === 'skip') {
      db.skipStep()
      setMsg('Moved the demo one step forward.')
    } else {
      db.triggerInvite()
      setMsg('Dana’s invite is on its way.')
    }
  }
  return (
    <Panel className="stack">
      <h2 style={{ fontSize: 'var(--fs-md)' }}>Demo controls</h2>
      <p className="muted">Mock mode only. These drive the scripted demo run.</p>
      <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
        <GelButton variant="white" size="sm" onClick={() => run('skip')}>
          Skip ahead one step
        </GelButton>
        <GelButton variant="white" size="sm" onClick={() => run('invite')}>
          Send Dana’s invite
        </GelButton>
        <GelButton variant="danger" size="sm" onClick={() => run('reset')}>
          Reset the demo
        </GelButton>
      </div>
      <p role="status" className="muted">
        {msg}
      </p>
    </Panel>
  )
}

export default function Settings() {
  const me = useMe()
  const cfg = useConfig()
  const prefs = usePrefs()
  const qc = useQueryClient()
  const nav = useNavigate()
  const [pushState, setPushState] = useState<string>('')

  useEffect(() => {
    document.title = 'Settings'
  }, [])

  const save = useMutation({
    mutationFn: (profile: Profile) => api.saveProfile({ profile }),
    onSuccess: (u) => qc.setQueryData(keys.me, u),
  })

  if (me.isPending)
    return (
      <>
        <TopBar title="Settings" back="/home" />
        <Page>
          <Loading />
        </Page>
      </>
    )
  if (me.isError)
    return (
      <>
        <TopBar title="Settings" back="/home" />
        <Page>
          <ErrorState error={me.error} onRetry={() => me.refetch()} />
        </Page>
      </>
    )

  const u = me.data
  const p = u.profile
  const patch = (x: Partial<Profile>) => save.mutate({ ...p, ...x })

  return (
    <>
      <TopBar title="Settings" back="/home" />
      <Page>
        <div className="stack">
          <Panel className="stack">
            <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
              <Avatar user={u} size={52} />
              <div>
                <h1 style={{ fontSize: 'var(--fs-xl)' }}>{u.display_name || 'You'}</h1>
                <p className="muted num">{u.telegram_username ? `@${u.telegram_username} on Telegram` : u.phone_masked}</p>
              </div>
            </div>
            <dl style={{ display: 'grid', gap: 6, margin: 0 }}>
              <div>
                <dt className="muted" style={{ fontSize: 'var(--fs-sm)' }}>
                  Drop-off
                </dt>
                <dd style={{ margin: 0 }}>{p.dropoff_default?.label ?? 'Not set'}</dd>
              </div>
              <div>
                <dt className="muted" style={{ fontSize: 'var(--fs-sm)' }}>
                  Diet and foods to avoid
                </dt>
                <dd style={{ margin: 0 }}>{[...p.diet, ...p.avoid.map((a) => `no ${a}`)].join(', ') || 'Nothing noted'}</dd>
              </div>
              <div>
                <dt className="muted" style={{ fontSize: 'var(--fs-sm)' }}>
                  Default tip
                </dt>
                <dd style={{ margin: 0 }} className="num">
                  {money(p.default_tip_cents)}
                </dd>
              </div>
            </dl>
            <p className="muted" style={{ fontSize: 'var(--fs-sm)' }}>
              The Ferry bot uses these answers when you message it on Telegram.
            </p>
            <GelLink to="/welcome?step=0&next=%2Fsettings" variant="white" size="sm">
              Edit my answers
            </GelLink>
          </Panel>

          <Panel>
            <h2 style={{ fontSize: 'var(--fs-md)' }}>Notifications</h2>
            <SwitchRow
              label="App notifications"
              help={pushState || 'When a ferry opens, your food moves, or someone invites you.'}
              checked={p.notify_push}
              onChange={async (v) => {
                patch({ notify_push: v })
                if (v) {
                  const r = await enablePush().catch(() => 'unsupported' as const)
                  setPushState(r === 'on' ? 'On for this device.' : r === 'denied' ? 'Blocked in your browser settings. You’ll still see banners in the app.' : 'This browser doesn’t support push. You’ll still see banners in the app.')
                }
              }}
            />
            <SwitchRow label="Messages from Ferry" help="Updates in your Telegram groups and chats." checked={p.notify_messages} onChange={(v) => patch({ notify_messages: v })} />
          </Panel>

          <Panel>
            <h2 style={{ fontSize: 'var(--fs-md)' }}>Ferrying</h2>
            <SwitchRow label="I’m open to ferrying for others" checked={p.runner_opt_in} onChange={(v) => patch({ runner_opt_in: v })} />
            <SwitchRow label="Share my location while I’m ferrying" help="Only during an active run." checked={p.share_location_when_running} onChange={(v) => patch({ share_location_when_running: v })} />
          </Panel>

          <Panel className="stack">
            <h2 style={{ fontSize: 'var(--fs-md)' }}>Sound and motion</h2>
            <SwitchRow
              label="Water-drop sounds"
              help="Soft sounds when an order is confirmed or food arrives."
              checked={prefs.sound}
              onChange={(v) => {
                setPrefs({ sound: v })
                if (v) sound.preview()
              }}
            />
            <div className="stack" style={{ gap: 8 }}>
              <strong id="motion-h">Motion</strong>
              <div className={chipGroupClass} role="radiogroup" aria-labelledby="motion-h">
                <Chip role="radio" selected={prefs.motion === 'system'} onClick={() => setPrefs({ motion: 'system' })}>
                  Follow my device
                </Chip>
                <Chip role="radio" selected={prefs.motion === 'reduced'} onClick={() => setPrefs({ motion: 'reduced' })}>
                  Reduce motion
                </Chip>
              </div>
            </div>
          </Panel>

          {save.isError && (
            <p className={c.error} role="alert">
              {save.error instanceof ApiError ? save.error.message : 'That setting didn’t save.'}
            </p>
          )}

          {env.isMock && cfg.data?.demo_mode && <DemoControls />}

          <GelButton
            variant="danger"
            onClick={() => {
              session.set(null)
              realtime.disconnect()
              qc.clear()
              nav('/', { replace: true })
            }}
          >
            Sign out
          </GelButton>
          <p className={c.note}>
            <Link to="/">About Ferry</Link>
          </p>
        </div>
      </Page>
    </>
  )
}
