import { PersonSimpleWalk } from '@phosphor-icons/react'
import { useEffect } from 'react'
import { Link, Navigate, useParams } from 'react-router-dom'
import { useCircles, useMe, useMenu, useRun } from '../api/hooks'
import { clock, countdown, plural } from '../lib/format'
import { useNow } from '../lib/time'
import { CartBar } from '../order/CartBar'
import { Menu } from '../order/Menu'
import { LazyMap } from '../map/LazyMap'
import { Page, TopBar } from '../shell/TopBar'
import { Avatar, FoodBubble } from '../ui/Bubble'
import { CountdownRing } from '../ui/CountdownRing'
import { GelLink } from '../ui/GelButton'
import { Empty, ErrorState, Loading } from '../ui/States'
import { Panel } from '../ui/Surface'
import c from './common.module.css'
import s from './JoinRun.module.css'

// The page the agent links to from group chats: {web_url}/r/{run_id}.
export default function JoinRun() {
  const { runId = '' } = useParams()
  const me = useMe()
  const run = useRun(runId)
  const menu = useMenu(run.data?.restaurant.id)
  const circles = useCircles()
  const now = useNow(1000)

  useEffect(() => {
    if (run.data) document.title = `Ferry to ${run.data.restaurant.name} with ${run.data.runner.display_name}`
  }, [run.data])

  if (run.isPending)
    return (
      <>
        <TopBar title="Ferry" back="/home" />
        <Page>
          <Loading label="Loading this ferry" />
        </Page>
      </>
    )
  if (run.isError)
    return (
      <>
        <TopBar title="Ferry" back="/home" />
        <Page>
          <Panel>
            <ErrorState error={run.error} onRetry={() => run.refetch()} title="We couldn’t open this ferry" />
          </Panel>
        </Page>
      </>
    )

  const r = run.data
  if (me.data && r.runner.id === me.data.id) return <Navigate to={`/run/${r.id}`} replace />
  const left = Date.parse(r.window_closes_at) - now
  const open = r.status === 'open' && left > 0
  const myOrder = r.orders.find((o) => o.customer.id === me.data?.id)
  const circle = circles.data?.find((x) => x.id === r.circle_id)

  return (
    <>
      <TopBar title={r.restaurant.name} back="/home" />
      <Page wide>
        <div className={c.split}>
          <div className={c.sticky}>
            <Panel className={s.hero}>
              <div className={s.heroTop}>
                <CountdownRing closesAt={r.window_closes_at} size={92} label={r.restaurant.name}>
                  <FoodBubble size={74} cuisine={r.restaurant.cuisine} imageUrl={r.restaurant.image_url} name={r.restaurant.name} />
                </CountdownRing>
                <div className={s.heroText}>
                  <h1>
                    {r.runner.display_name} is going to {r.restaurant.name}
                  </h1>
                  <p className="muted">
                    Leaves at {clock(r.leaving_at)}
                    {circle ? ` from ${circle.name}` : ''}.
                  </p>
                </div>
              </div>
              <div className={s.facts}>
                <div>
                  <span className={s.factLabel}>{open ? 'Time left' : 'Window'}</span>
                  <span className={`${s.factValue} num`} aria-live="off">
                    {open ? countdown(left) : 'Closed'}
                  </span>
                </div>
                <div>
                  <span className={s.factLabel}>Orders in</span>
                  <span className={`${s.factValue} num`}>{r.order_count}</span>
                </div>
                <div>
                  <span className={s.factLabel}>Captain</span>
                  <span className={s.runner}>
                    <Avatar user={r.runner} size={28} label={false} />
                    {r.runner.display_name}
                  </span>
                </div>
              </div>
              {/* The whole plan up front: when they leave, when they're back, where, and how far they'll walk. */}
              <div className={s.plan}>
                <h2>The plan</h2>
                <ol className={s.planSteps}>
                  <li>
                    <strong className="num">{clock(r.leaving_at)}</strong> leaves for {r.restaurant.name}
                  </li>
                  {r.returning_at && (
                    <li>
                      <strong className="num">{clock(r.returning_at)}</strong> back {r.return_place ? `at ${r.return_place.label}` : ''}
                    </li>
                  )}
                  {r.dropoff_radius_m && (
                    <li>
                      Drops off within <strong className="num">{r.dropoff_radius_m} m</strong> of there
                    </li>
                  )}
                </ol>
                {r.return_place && r.dropoff_radius_m && (
                  <div className={s.planMap}>
                    <LazyMap
                      label={`Map of where ${r.runner.display_name} drops off: within ${r.dropoff_radius_m} m of ${r.return_place.label}`}
                      center={r.return_place.location}
                      zoom={r.dropoff_radius_m > 600 ? 14.2 : 15}
                      interactive={false}
                      zone={{ center: r.return_place.location, radius_m: r.dropoff_radius_m }}
                      markers={[{ id: 'back', at: r.return_place.location, kind: 'dropoff', label: r.return_place.label }]}
                    />
                  </div>
                )}
                <p className="muted">
                  {r.runner.display_name} carries the food. Ferry holds your money until it’s in your hands.
                </p>
              </div>
              {myOrder && (
                <GelLink to={`/o/${myOrder.id}`} variant="white" block icon={<PersonSimpleWalk size={20} aria-hidden />}>
                  Track my order
                </GelLink>
              )}
            </Panel>
          </div>
          <div>
            {open ? (
              menu.isPending ? (
                <Loading label="Loading the menu" rows={5} />
              ) : menu.isError ? (
                <Panel>
                  <ErrorState error={menu.error} onRetry={() => menu.refetch()} />
                </Panel>
              ) : (
                <>
                  <h2 className={s.menuHead}>{myOrder ? 'Add another order' : 'Add your order'}</h2>
                  <Menu menu={menu.data} cartKey={r.restaurant.id} />
                  <CartBar cartKey={r.restaurant.id} to={`/r/${r.id}/checkout`} label={`Order with ${r.runner.display_name}`} />
                </>
              )
            ) : (
              <Panel>
                <Empty icon={<PersonSimpleWalk size={28} color="var(--sky-700)" aria-hidden />} title={myOrder ? 'You’re in' : `${r.runner.display_name} already left`}>
                  {myOrder
                    ? `${r.runner.display_name} has your order. Track it as it walks to you.`
                    : `This ferry closed its window with ${plural(r.order_count, 'order')}. Check what else is leaving soon.`}
                </Empty>
                {!myOrder && (
                  <div style={{ display: 'grid', justifyItems: 'center' }}>
                    <Link to="/home">See ferries leaving soon</Link>
                  </div>
                )}
              </Panel>
            )}
          </div>
        </div>
      </Page>
    </>
  )
}
