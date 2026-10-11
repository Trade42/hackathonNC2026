import { Basket, Wallet as WalletIcon } from '@phosphor-icons/react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError } from '../api/client'
import { api } from '../api/endpoints'
import { keys, setWallet, useConfig, useMe, usePlaceOrder, usePlaces, useRestaurants, useRun, useWallet } from '../api/hooks'
import type { Place } from '../api/types'
import { clock, countdown, money } from '../lib/format'
import { ferryFeeCents, runnerPayCents } from '../lib/pricing'
import { knownPlaces } from '../lib/places'
import { LocationPicker, type PlaceZone } from '../ui/LocationPicker'
import { currentLocation, locationAllowed, metersBetween, placeAt } from '../lib/geocode'
import { useNow } from '../lib/time'
import { Qty } from '../order/ItemSheet'
import { Page, TopBar } from '../shell/TopBar'
import { cartStore, useCart } from '../state/cart'
import { Chip, chipGroupClass } from '../ui/Chip'
import { Field } from '../ui/Field'
import { GelButton, GelLink } from '../ui/GelButton'
import { pop } from '../ui/pops'
import { Empty, Loading } from '../ui/States'
import { Panel } from '../ui/Surface'
import c from './common.module.css'
import s from './Checkout.module.css'

const TAX_ESTIMATE = 0.075 // Chapel Hill sales tax; the server's number is final.

function useSessionState<T>(key: string, init: T) {
  const [v, setV] = useState<T>(() => {
    try {
      const raw = sessionStorage.getItem(key)
      return raw ? (JSON.parse(raw) as T) : init
    } catch {
      return init
    }
  })
  useEffect(() => {
    try {
      sessionStorage.setItem(key, JSON.stringify(v))
    } catch {
      /* memory only */
    }
  }, [key, v])
  return [v, setV] as const
}

const NEEDED = [
  { label: 'In 30 min', min: 30 },
  { label: 'In an hour', min: 60 },
  { label: 'In 2 hours', min: 120 },
]

export default function Checkout() {
  const { runId, areaId, restaurantId: ridParam } = useParams()
  const requestMode = !runId
  const nav = useNavigate()
  const qc = useQueryClient()
  const me = useMe()
  const places = usePlaces()
  const cfg = useConfig()
  const wallet = useWallet()
  const run = useRun(runId)
  const rests = useRestaurants({ area_id: areaId })
  const now = useNow(1000)

  const restaurantId = run.data?.restaurant.id ?? ridParam ?? ''
  const restaurant = run.data?.restaurant ?? rests.data?.find((r) => r.id === ridParam)
  const { lines, subtotal, count } = useCart(restaurantId)
  const scope = runId ?? ridParam ?? 'x'
  const [tip, setTip] = useSessionState<number | null>(`ferry.tip.${scope}`, null)
  const [dropoff, setDropoff] = useSessionState<PlaceZone | null>(`ferry.dropoff2.${scope}`, null)
  const [here, setHere] = useState<Place | null>(null)
  const [neededMin, setNeededMin] = useSessionState(`ferry.needed.${scope}`, 60)

  const profile = me.data?.profile
  const tipCents = tip ?? profile?.default_tip_cents ?? 200
  // Where the food goes: your choice, else where you are now (if location is
  // already allowed), else your default drop-off.
  const known = knownPlaces(places.data)
  const zone: PlaceZone = dropoff ?? { place: here ?? profile?.dropoff_default ?? known[0], radius_m: 100 }
  const place = zone.place
  const savedPlaces = [...(profile?.dropoff_default && !known.some((d) => d.id === profile.dropoff_default?.id) ? [profile.dropoff_default] : []), ...known]
  useEffect(() => {
    let alive = true
    void (async () => {
      if (dropoff || !(await locationAllowed())) return
      const at = await currentLocation().catch(() => null)
      if (!at || !alive) return
      const p = await placeAt(at)
      if (alive) setHere({ ...p, label: p.label === 'Dropped pin' ? 'Your location' : p.label })
    })()
    return () => {
      alive = false
    }
    // Only on arrival.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])
  // How far that is from where the runner heads back, against how far they'll walk.
  const runPlan = run.data
  const offBy =
    runPlan?.return_place && runPlan.dropoff_radius_m ? Math.round(metersBetween(place.location, runPlan.return_place.location) - runPlan.dropoff_radius_m) : null
  const taxEst = Math.round(subtotal * TAX_ESTIMATE)
  const estimate = subtotal + taxEst + ferryFeeCents() + tipCents
  const orderLines = lines.map((l) => ({ menu_item_id: l.menu_item_id, qty: l.qty, choice_ids: l.choice_ids, ...(l.note ? { note: l.note } : {}) }))
  const quote = useQuery({
    queryKey: ['quote', runId, orderLines, tipCents, place, zone.radius_m],
    queryFn: () => api.quoteOrder(runId!, { lines: orderLines, tip_cents: tipCents, dropoff: place, dropoff_radius_m: zone.radius_m }),
    enabled: !requestMode && !!runId && count > 0,
    staleTime: 15_000,
  })
  const priced = quote.data ?? { subtotal_cents: subtotal, tax_cents: taxEst, ferry_fee_cents: ferryFeeCents(), tip_cents: tipCents, total_cents: estimate }

  const place_ = usePlaceOrder(runId ?? '')
  const topup = useMutation({
    mutationFn: (amount: number) => api.demoTopup(amount),
    onSuccess: (w) => setWallet(qc, w),
  })
  const request = useMutation({
    mutationFn: () =>
      api.requestFerry({
        restaurant_id: restaurantId,
        lines: orderLines,
        tip_cents: tipCents,
        dropoff: place,
        dropoff_radius_m: zone.radius_m,
        needed_by: new Date(Date.now() + neededMin * 60e3).toISOString(),
      }),
    onSuccess: (r) => {
      if (r.run_id) nav(`/r/${r.run_id}/checkout`, { replace: true })
    },
  })

  useEffect(() => {
    document.title = requestMode ? 'Request a ferry' : 'Checkout'
  }, [requestMode])

  const submit = () => {
    if (!quote.data) return
    place_.mutate(
      {
        lines: orderLines,
        tip_cents: tipCents,
        dropoff: place,
        dropoff_radius_m: zone.radius_m,
      },
      {
        onSuccess: (order) => {
          cartStore.clear(restaurantId)
          pop(null, { label: 'Order confirmed', sound: 'chime', size: 80 })
          qc.invalidateQueries({ queryKey: keys.myOrders })
          nav(`/o/${order.id}`, { replace: true, state: { placed: true } })
        },
      },
    )
  }

  const backTo = runId ? `/r/${runId}` : `/food/${areaId}/${ridParam}`
  const title = requestMode ? 'Request a ferry' : 'Checkout'

  if (!requestMode && run.isPending)
    return (
      <>
        <TopBar title={title} back={backTo} />
        <Page>
          <Loading />
        </Page>
      </>
    )

  if (!count && !place_.isSuccess)
    return (
      <>
        <TopBar title={title} back={backTo} />
        <Page>
          <Panel>
            <Empty icon={<Basket size={28} color="var(--sky-700)" aria-hidden />} title="Your order is empty" action={<GelLink to={backTo}>Back to the menu</GelLink>}>
              Add something from the menu first.
            </Empty>
          </Panel>
        </Page>
      </>
    )

  const r = run.data
  const left = r ? Date.parse(r.window_closes_at) - now : 0
  const err = place_.error instanceof ApiError ? place_.error : null
  const windowClosed = !requestMode && !!r && (r.status !== 'open' || left <= 0 || err?.code === 'window_closed')
  const balance = wallet.data?.balance_cents
  const short = balance !== undefined && balance < priced.total_cents

  if (request.isSuccess && !request.data.run_id)
    return (
      <>
        <TopBar title={title} back={backTo} />
        <Page>
          <Panel>
            <Empty icon={<Basket size={28} color="var(--sky-700)" aria-hidden />} title="Looking for a nearby trip" action={<GelLink to="/home">Back home</GelLink>}>
              When a student heads to {restaurant?.name ?? 'the restaurant'}, Ferry can add your order to their run and let you know. Nothing is charged until then.
            </Empty>
          </Panel>
        </Page>
      </>
    )

  return (
    <>
      <TopBar title={title} back={backTo} />
      <Page wide>
        <div className={c.split}>
          <div className="stack">
            <Panel>
              <div className={s.head}>
                <h1>{restaurant?.name}</h1>
                {r && (
                  <p className="muted">
                    {r.runner.display_name} is going and will bring it to you.{' '}
                    {left > 0 ? (
                      <span className={left < 120e3 ? s.urgent : ''}>
                        Window closes in <span className="num">{countdown(left)}</span>.
                      </span>
                    ) : (
                      'The window has closed.'
                    )}
                  </p>
                )}
              </div>
              <ul className={s.lines}>
                {lines.map((l) => (
                  <li key={l.key} className={s.line}>
                    <div className={s.lineMain}>
                      <strong>{l.name}</strong>
                      {(l.choice_names.length > 0 || l.note) && <span>{[...l.choice_names, l.note && `“${l.note}”`].filter(Boolean).join(', ')}</span>}
                    </div>
                    <Qty value={l.qty} min={0} onChange={(q) => cartStore.setQty(restaurantId, l.key, q)} label={`Quantity of ${l.name}`} />
                    <span className={`${s.lineTotal} num`}>{money(l.unit_price_cents * l.qty)}</span>
                  </li>
                ))}
              </ul>
              <Link to={backTo} className={s.addMore}>
                Add more items
              </Link>
            </Panel>

            <Panel className="stack">
              <h2 className={s.h2}>Where should it go?</h2>
              <LocationPicker
                value={zone}
                onChange={setDropoff}
                heading="Deliver to"
                zoneLabel="Happy to meet within"
                sheetTitle="Where should your food go?"
                sheetSubtitle="Search, use your location, or drag the map. Then set how far you’ll walk to meet."
                saved={savedPlaces}
                minRadius={50}
                maxRadius={500}
              />
              {runPlan?.return_place && runPlan.returning_at && (
                <p className={s.plan}>
                  {runPlan.runner.display_name} heads back to {runPlan.return_place.label} around {clock(runPlan.returning_at)}
                  {runPlan.dropoff_radius_m ? ` and drops off within ${runPlan.dropoff_radius_m} m.` : '.'}
                </p>
              )}
              {offBy !== null && offBy > 0 && (
                <p className={s.far} role="status">
                  That’s about {offBy} m farther than {runPlan!.runner.display_name} walks. Pick a spot closer to {runPlan!.return_place!.label}, or meet there.
                </p>
              )}
            </Panel>

            {requestMode && (
              <Panel>
                <h2 className={s.h2} id="need-h">
                  Need it by
                </h2>
                <div className={chipGroupClass} role="radiogroup" aria-labelledby="need-h">
                  {NEEDED.map((n) => (
                    <Chip key={n.min} role="radio" selected={neededMin === n.min} onClick={() => setNeededMin(n.min)}>
                      {n.label}
                    </Chip>
                  ))}
                </div>
              </Panel>
            )}
          </div>

          <div className={c.sticky}>
            <Panel className="stack">
              <Field
                type="range"
                label={`Tip for ${r?.runner.display_name ?? 'your captain'}`}
                min={0}
                max={800}
                step={50}
                value={tipCents}
                onChange={(e) => setTip(Number(e.target.value))}
                aria-valuetext={money(tipCents)}
                help="All of it goes to the student who walks your food."
              />
              <dl className={s.summary}>
                <div>
                  <dt>Subtotal</dt>
                  <dd className="num">{money(priced.subtotal_cents)}</dd>
                </div>
                <div>
                  <dt>{requestMode ? 'Tax (estimate)' : 'Tax'}</dt>
                  <dd className="num">{money(priced.tax_cents)}</dd>
                </div>
                <div>
                  <dt>Ferry fee</dt>
                  <dd className="num">{money(priced.ferry_fee_cents)}</dd>
                </div>
                <div>
                  <dt>Tip</dt>
                  <dd className="num">{money(priced.tip_cents)}</dd>
                </div>
                <div className={s.total}>
                  <dt>{requestMode ? 'Estimated total' : 'Total before handoff'}</dt>
                  <dd className="num">{money(priced.total_cents)}</dd>
                </div>
              </dl>
              <p className={c.note}>
                {requestMode ? 'This is an estimate. You pay only when a captain accepts your request.' : quote.isPending ? 'Checking the exact total…' : 'This is the amount held in your wallet. Food under the receipt estimate is refunded.'}
                {' '}The captain earns {money(runnerPayCents(priced.ferry_fee_cents, priced.tip_cents))} including your tip.
              </p>
              <p className={s.carry}>
                {r ? `${r.runner.display_name} picks it up and walks it to ${place.label}.` : `Whoever picks up your request walks it to ${place.label}.`} Ferry holds your money until the food is in your hands.
              </p>

              <div className={s.wallet}>
                <WalletIcon size={22} weight="duotone" color="var(--sky-700)" aria-hidden />
                <div>
                  <strong>Pay from Ferry Wallet</strong>
                  <span>Sandbox money (Capital One Nessie)</span>
                </div>
                <span className="num">{balance !== undefined ? money(balance) : '...'}</span>
              </div>

              {windowClosed && (
                <div className={s.alert} role="alert">
                  <strong>{r?.runner.display_name ?? 'The captain'} already closed the window.</strong>
                  <span>Your cart is saved. Ask for a ferry and we’ll find the next person heading there.</span>
                  <GelLink to={`/food/${restaurant?.area_id ?? 'area_franklin'}/${restaurantId}/request`} variant="white" size="sm">
                    Request a ferry
                  </GelLink>
                </div>
              )}
              {(err?.code === 'insufficient_funds' || (short && !requestMode)) && (
                <div className={s.alert} role="alert">
                  <strong>Not enough in your Ferry Wallet</strong>
                  <span>
                    You have {money(balance ?? 0)} and this order needs {money(priced.total_cents)}.
                  </span>
                  {cfg.data?.demo_mode && (
                    <GelButton variant="white" size="sm" busy={topup.isPending} onClick={() => topup.mutate(2500)}>
                      Add $25 of demo money
                    </GelButton>
                  )}
                </div>
              )}
              {err && !['window_closed', 'insufficient_funds'].includes(err.code) && (
                <p className={c.error} role="alert">
                  {err.message}
                </p>
              )}
              {quote.isError && !requestMode && <p className={c.error} role="alert">Could not confirm the total. Try again before placing the order.</p>}
              {request.isError && (
                <p className={c.error} role="alert">
                  {request.error instanceof ApiError ? request.error.message : 'The request didn’t go through. Try again.'}
                </p>
              )}

              {requestMode ? (
                <GelButton size="lg" block busy={request.isPending} onClick={() => request.mutate()}>
                  Request a ferry
                </GelButton>
              ) : (
                <GelButton size="lg" block busy={place_.isPending} disabled={windowClosed || !quote.data || quote.isError} onClick={submit}>
                  Place order
                </GelButton>
              )}
            </Panel>
          </div>
        </div>
      </Page>
    </>
  )
}
