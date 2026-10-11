import { CaretRight, GearSix, Leaf, PencilSimple, Wallet } from '@phosphor-icons/react'
import { useQueryClient } from '@tanstack/react-query'
import { useEffect } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useImpact, useMe, useWallet } from '../api/hooks'
import { realtime } from '../api/realtime'
import { grams, money, plural } from '../lib/format'
import { Page, TopBar } from '../shell/TopBar'
import { session } from '../state/session'
import { AeroIcon } from '../ui/AeroIcon'
import { Avatar } from '../ui/Bubble'
import { GelButton } from '../ui/GelButton'
import { ErrorState, Loading, Skeleton } from '../ui/States'
import { Panel } from '../ui/Surface'
import s from './You.module.css'

// Your account at a glance: wallet, impact, settings.
export default function You() {
  const me = useMe()
  const wallet = useWallet()
  const impact = useImpact('me')
  const qc = useQueryClient()
  const nav = useNavigate()

  useEffect(() => {
    document.title = 'You on Ferry'
  }, [])

  if (me.isPending)
    return (
      <>
        <TopBar title="You" />
        <Page>
          <Loading />
        </Page>
      </>
    )
  if (me.isError)
    return (
      <>
        <TopBar title="You" />
        <Page>
          <ErrorState error={me.error} onRetry={() => me.refetch()} />
        </Page>
      </>
    )

  const u = me.data
  return (
    <>
      <TopBar title="You" />
      <Page>
        <div className="stack">
          <Panel className={s.profile}>
            <Avatar user={u} size={64} />
            <div className={s.who}>
              <h1>{u.display_name || 'You'}</h1>
              <p className="muted">{u.telegram_username ? `@${u.telegram_username} on Telegram` : u.phone_masked}</p>
              <p className="muted">Drop-off: {u.profile.dropoff_default?.label ?? 'not set'}</p>
            </div>
            <Link to="/welcome?step=0&next=%2Fyou" className={s.edit} aria-label="Edit your answers">
              <PencilSimple size={20} aria-hidden />
            </Link>
          </Panel>

          <div className={s.cards}>
            <Link to="/wallet" className={s.card}>
              <AeroIcon icon={Wallet} tone="sky" size={48} />
              <span className={s.cardText}>
                <small>Ferry Wallet</small>
                {wallet.data ? <strong className="num">{money(wallet.data.balance_cents)}</strong> : <Skeleton h={28} w={90} />}
                <small>Sandbox money from Capital One Nessie</small>
              </span>
              <CaretRight size={18} aria-hidden className={s.caret} />
            </Link>
            <Link to="/impact" className={s.card}>
              <AeroIcon icon={Leaf} tone="leaf" size={48} />
              <span className={s.cardText}>
                <small>CO2 you saved</small>
                {impact.data ? <strong className="num co2">{grams(impact.data.co2_saved_g)}</strong> : <Skeleton h={28} w={90} />}
                <small>{impact.data ? `${plural(impact.data.deliveries, 'walked delivery', 'walked deliveries')}` : ' '}</small>
              </span>
              <CaretRight size={18} aria-hidden className={s.caret} />
            </Link>
          </div>

          <Panel pad="flush">
            <Link to="/settings" className={s.row}>
              <GearSix size={22} aria-hidden />
              <span>Settings</span>
              <CaretRight size={18} aria-hidden className={s.caret} />
            </Link>
          </Panel>

          <GelButton
            variant="danger"
            onClick={() => {
              session.set(null)
              realtime.disconnect()
              qc.clear()
              nav('/signin', { replace: true })
            }}
          >
            Sign out
          </GelButton>
        </div>
      </Page>
    </>
  )
}
