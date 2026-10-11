import { HandWaving, Leaf, MagnifyingGlass, PersonSimpleWalk } from '@phosphor-icons/react'
import { useQueries, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState, useSyncExternalStore } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { api } from '../api/endpoints'
import { keys, useImpact, useMe, useMyOrders, useOpenRuns, useRestaurants } from '../api/hooks'
import type { Order, Run, RunDetail } from '../api/types'
import { grams } from '../lib/format'
import { ACTIVE } from '../lib/orders'
import { CAMPUS_CENTER } from '../lib/places'
import { useNow } from '../lib/time'
import { refillCart } from '../lib/usual'
import { LazyMap } from '../map/LazyMap'
import { Page, TopBar } from '../shell/TopBar'
import { myRuns, rememberRun } from '../state/myRuns'
import { AeroIcon } from '../ui/AeroIcon'
import { Bubble } from '../ui/Bubble'
import { cuisineFamily } from '../ui/CuisineArt'
import { CUISINES } from '../ui/cuisines'
import f from '../ui/Feed.module.css'
import { GelButton, GelLink } from '../ui/GelButton'
import { LiveOrderCard } from '../ui/LiveOrderCard'
import { RestaurantCard } from '../ui/RestaurantCard'
import { RunCarousel } from '../ui/RunCarousel'
import { RunnerMark } from '../ui/RunnerMark'
import { Empty, ErrorState, Skeleton } from '../ui/States'
import { Panel } from '../ui/Surface'
import c from './common.module.css'
import s from './Home.module.css'

function useMyActiveRuns(meId: string | undefined, openIds: string[]) {
  const ids = Array.from(new Set([...myRuns(), ...openIds]))
  const results = useQueries({ queries: ids.map((id) => ({ queryKey: keys.run(id), queryFn: () => api.run(id), enabled: !!meId })) })
  return results
    .map((r) => r.data)
    .filter((r): r is RunDetail => !!r && r.runner.id === meId && !['delivered', 'cancelled'].includes(r.status))
}

const desktopMq = typeof window !== 'undefined' ? window.matchMedia('(min-width: 1024px) and (min-height: 501px)') : null
const useDesktop = () =>
  useSyncExternalStore(
    (fn) => {
      desktopMq?.addEventListener('change', fn)
      return () => desktopMq?.removeEventListener('change', fn)
    },
    () => !!desktopMq?.matches,
  )

// Ferry's one suggestion: a student's open run to a place you like. When you've
// ordered there before, it offers your usual. Built only from real data.
function Suggestion({ runs, orders, favorites, restName }: { runs: Run[]; orders: Order[]; favorites: string[]; restName: (id: string) => string | undefined }) {
  const qc = useQueryClient()
  const nav = useNavigate()
  const now = useNow(15_000)
  const [busy, setBusy] = useState(false)
  const past = orders.filter((o) => o.status === 'delivered' || o.status === 'settled')
  const run = runs.find((r) => Date.parse(r.window_closes_at) > now && (past.some((o) => o.restaurant_id === r.restaurant.id) || favorites.includes(r.restaurant.id)))
  if (!run) return null
  const usual = past.find((o) => o.restaurant_id === run.restaurant.id)
  const mins = Math.max(1, Math.round((Date.parse(run.window_closes_at) - now) / 60e3))
  const items = usual?.lines.map((l) => l.name).join(', ')
  return (
    <section className={s.assist} aria-labelledby="assist-h">
      <p className={s.assistWho}>
        <RunnerMark size={22} />
        <span id="assist-h">Ferry, your order assistant</span>
      </p>
      <p className={s.assistText}>
        {run.runner.display_name} is heading to {restName(run.restaurant.id) ?? run.restaurant.name}, and you have {mins} min to add an order.
        {usual ? ` Want your usual ${items}?` : ' Want anything?'}
      </p>
      <div className={s.assistActions}>
        {usual ? (
          <GelButton
            size="sm"
            busy={busy}
            onClick={async () => {
              setBusy(true)
              try {
                const n = await refillCart(qc, run.restaurant.id, usual)
                nav(n ? `/r/${run.id}/checkout` : `/r/${run.id}`)
              } finally {
                setBusy(false)
              }
            }}
          >
            Add my usual
          </GelButton>
        ) : null}
        <GelLink to={`/r/${run.id}`} size="sm" variant="white">
          See the menu
        </GelLink>
      </div>
    </section>
  )
}

export default function Home() {
  const desktop = useDesktop()
  const me = useMe()
  const runs = useOpenRuns()
  const orders = useMyOrders()
  const impact = useImpact('me')
  const rests = useRestaurants({})
  const restName = (id: string) => rests.data?.find((r) => r.id === id)?.name

  useEffect(() => {
    document.title = 'Ferry'
  }, [])

  const mine = runs.data?.filter((r) => r.runner.id === me.data?.id) ?? []
  const mineIds = mine.map((r) => r.id).join(',')
  useEffect(() => {
    if (mineIds) mineIds.split(',').forEach(rememberRun)
  }, [mineIds])
  const others = runs.data?.filter((r) => r.runner.id !== me.data?.id) ?? []
  const myActive = useMyActiveRuns(me.data?.id, mine.map((r) => r.id))
  const active = orders.data?.filter((o) => ACTIVE.includes(o.status)) ?? []
  const first = me.data?.display_name?.split(' ')[0]
  const families = new Set((rests.data ?? []).map((r) => cuisineFamily(r.cuisine)))
  const cuisines = CUISINES.filter((c) => families.has(c.family))
  const nearby = [...(rests.data ?? [])].sort((a, b) => Number(b.is_open) - Number(a.is_open) || (a.walk_minutes_from_dropoff ?? 99) - (b.walk_minutes_from_dropoff ?? 99))

  return (
    <>
      <TopBar brand title="Home" />
      <Page wide>
        <div className={s.layout}>
          <div className={s.main}>
            <h1 className={s.hello}>{first ? `Hungry, ${first}?` : 'Hungry?'}</h1>

            {/* The two things you do in Ferry. */}
            <div className={s.bigActions}>
              <Link to="/search?request=1" className={`${s.big} ${s.bigRequest}`}>
                <span className={s.bigIcon} aria-hidden>
                  <HandWaving size={30} weight="duotone" />
                </span>
                <strong>Request a ferry</strong>
                <span>A student heading out brings you food</span>
              </Link>
              <Link to="/run/new" className={`${s.big} ${s.bigGo}`}>
                <span className={s.bigIcon} aria-hidden>
                  <PersonSimpleWalk size={30} weight="duotone" />
                </span>
                <strong>I’m going somewhere</strong>
                <span>Bring food back for students on your way</span>
              </Link>
            </div>

            <section className={c.section} aria-labelledby="coming-h">
              <div className={c.sectionHead}>
                <h2 id="coming-h">Coming to you</h2>
                {active.length > 0 && <Link to="/orders">All orders</Link>}
              </div>
              {active.length > 0 ? (
                <div className="stack">
                  {active.map((o) => (
                    <LiveOrderCard key={o.id} order={o} restaurant={restName(o.restaurant_id)} />
                  ))}
                </div>
              ) : (
                <p className={s.nothing}>Nothing on its way yet. Request a ferry, or add an order to an open trip below.</p>
              )}
            </section>

            {me.data && (
              <Suggestion runs={others} orders={orders.data ?? []} favorites={me.data.profile.favorite_restaurant_ids} restName={restName} />
            )}

            {myActive.map((r) => (
              <Panel key={r.id} className={s.runner}>
                <RunnerMark size={48} animate="loop" />
                <div className={c.rowMain}>
                  <h2 style={{ fontSize: 'var(--fs-lg)' }}>You’re bringing food from {r.restaurant.name}</h2>
                  <p>{r.orders.length ? `${r.orders.length} ${r.orders.length === 1 ? 'person has' : 'people have'} ordered.` : 'No orders yet.'}</p>
                </div>
                <GelLink to={`/run/${r.id}`} size="sm" aria-label={`Captain mode for ${r.restaurant.name}`}>
                  Captain mode
                </GelLink>
              </Panel>
            ))}

            <section className={c.section} aria-labelledby="soon-h">
              <div className={c.sectionHead}>
                <h2 id="soon-h">Students heading out</h2>
              </div>
              {runs.isPending ? (
                <div className={s.carouselSkeleton} aria-hidden>
                  <Skeleton h={200} />
                  <Skeleton h={200} />
                </div>
              ) : runs.isError ? (
                <Panel>
                  <ErrorState error={runs.error} onRetry={() => runs.refetch()} />
                </Panel>
              ) : others.length === 0 ? (
                <Panel>
                  <Empty icon={<PersonSimpleWalk size={28} color="var(--sky-700)" aria-hidden />} title="Nobody’s heading out right now">
                    When a student heads out for food, the open trip shows up here. Or request a ferry and someone heading that way can bring it.
                  </Empty>
                </Panel>
              ) : (
                <RunCarousel runs={others} />
              )}
            </section>

            <section className={c.section} aria-labelledby="browse-h">
              <div className={c.sectionHead}>
                <h2 id="browse-h">Browse food</h2>
                <Link to="/search">See all</Link>
              </div>
              <Link to="/search" className={s.search}>
                <MagnifyingGlass size={20} aria-hidden />
                Search restaurants or cuisines
              </Link>
              {cuisines.length > 0 && (
                <ul className={s.cuisines} aria-label="Cuisines">
                  {cuisines.map((cu) => (
                    <li key={cu.family}>
                      <Link to={`/search?cuisine=${cu.family}`} className={s.cuisine}>
                        <AeroIcon icon={cu.icon} tone={cu.tone} size={52} />
                        <span>{cu.label}</span>
                      </Link>
                    </li>
                  ))}
                </ul>
              )}
              {rests.isPending ? (
                <div className="stack" aria-hidden>
                  <Skeleton h={84} />
                  <Skeleton h={84} />
                  <Skeleton h={84} />
                </div>
              ) : rests.isError ? (
                <Panel>
                  <ErrorState error={rests.error} onRetry={() => rests.refetch()} />
                </Panel>
              ) : (
                <ul className={f.restList} aria-label="Restaurants near you">
                  {nearby.slice(0, 6).map((r) => (
                    <RestaurantCard key={r.id} r={r} run={others.find((x) => x.restaurant.id === r.id)} />
                  ))}
                </ul>
              )}
            </section>
          </div>

          <aside className={s.side} aria-labelledby="impact-h">
            <Link to="/impact" className={s.impact}>
              <Bubble size={56} color="#2E9A3B" seed="impact">
                <Leaf size={28} weight="fill" aria-hidden />
              </Bubble>
              <span className={s.impactText}>
                <span id="impact-h" className={s.impactLabel}>
                  Your CO2 saved
                </span>
                {impact.data ? (
                  <span className={`${s.impactNum} num co2`}>{grams(impact.data.co2_saved_g)}</span>
                ) : (
                  <Skeleton h={30} w={90} />
                )}
                <span className={s.impactSub}>{impact.data ? `${impact.data.deliveries} walked ${impact.data.deliveries === 1 ? 'delivery' : 'deliveries'}` : ' '}</span>
              </span>
            </Link>
            {desktop && (
              <div className={s.mapCard}>
                <h2 className={s.mapTitle}>Open ferries near you</h2>
                <div className={s.mapBox}>
                  <LazyMap
                    label="Map of restaurants with an open ferry and your drop-off"
                    center={me.data?.profile.dropoff_default?.location ?? CAMPUS_CENTER}
                    zoom={14.5}
                    interactive
                    markers={[
                      ...(me.data?.profile.dropoff_default ? [{ id: 'drop', at: me.data.profile.dropoff_default.location, kind: 'dropoff' as const, label: 'Your drop-off' }] : []),
                      ...others.map((r) => ({ id: r.id, at: r.restaurant.location, kind: 'restaurant' as const, label: `${r.runner.display_name}: ${r.restaurant.name}` })),
                    ]}
                    fit={[...(me.data?.profile.dropoff_default ? [me.data.profile.dropoff_default.location] : []), ...others.map((r) => r.restaurant.location)]}
                  />
                </div>
              </div>
            )}
          </aside>
        </div>
      </Page>
    </>
  )
}
