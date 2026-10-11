import { Basket, Check, CookingPot, HandHeart, PersonSimpleWalk, Receipt } from '@phosphor-icons/react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useLocation, useParams } from 'react-router-dom'
import { ApiError } from '../api/client'
import { api } from '../api/endpoints'
import { keys, upsertOrder, useHandoff, useMe, useOrder, useRestaurants, useRun, useTrack } from '../api/hooks'
import type { Order, OrderStatus } from '../api/types'
import { ago, clock, grams, miles, money } from '../lib/format'
import { useNow } from '../lib/time'
import { LazyMap, type MapMarker } from '../map/LazyMap'
import { Page, TopBar } from '../shell/TopBar'
import { Celebration } from '../ui/Celebration'
import { Avatar } from '../ui/Bubble'
import { GelButton } from '../ui/GelButton'
import { pop } from '../ui/pops'
import { Sheet } from '../ui/Sheet'
import { ErrorState, Loading } from '../ui/States'
import { glassClass, Panel } from '../ui/Surface'
import c from './common.module.css'
import s from './OrderTrack.module.css'

const STEPS: { key: OrderStatus; label: string; Icon: typeof Check }[] = [
  { key: 'funded', label: 'Ordered', Icon: Basket },
  { key: 'purchased', label: 'Bought', Icon: CookingPot },
  { key: 'picked_up', label: 'On the way', Icon: PersonSimpleWalk },
  { key: 'delivered', label: 'Delivered', Icon: HandHeart },
]
const rank: Record<OrderStatus, number> = { funded: 0, purchased: 1, picked_up: 2, delivered: 3, settled: 3, cancelled: -1, refunded: -1 }
const STALE_MS = 60_000

function useThrottled<T>(value: T, ms: number) {
  const [v, setV] = useState(value)
  const last = useRef(0)
  useEffect(() => {
    const now = Date.now()
    if (now - last.current >= ms) {
      last.current = now
      setV(value)
      return
    }
    const t = window.setTimeout(() => {
      last.current = Date.now()
      setV(value)
    }, ms - (now - last.current))
    return () => window.clearTimeout(t)
  }, [value, ms])
  return v
}

function Timeline({ order }: { order: Order }) {
  const cur = rank[order.status]
  return (
    <ol className={s.timeline} aria-label="Order progress">
      {STEPS.map(({ key, label, Icon }, i) => {
        const at = order.timeline.find((t) => t.status === key || (key === 'delivered' && t.status === 'settled'))?.at
        const done = i <= cur
        return (
          <li key={key} className={s.step} data-done={done} data-current={i === cur} aria-current={i === cur ? 'step' : undefined}>
            <span className={s.dot}>{done ? <Check size={14} weight="bold" aria-hidden /> : <Icon size={14} aria-hidden />}</span>
            <span>{label}</span>
            {at && <span className="num">{clock(at)}</span>}
          </li>
        )
      })}
    </ol>
  )
}

export default function OrderTrack() {
  const { orderId = '' } = useParams()
  const loc = useLocation()
  const justPlaced = (loc.state as { placed?: boolean } | null)?.placed
  const qc = useQueryClient()
  const me = useMe()
  const order = useOrder(orderId)
  const o = order.data
  const run = useRun(o?.run_id)
  const moving = !!o && ['purchased', 'picked_up'].includes(o.status)
  const track = useTrack(o?.run_id, !!o && rank[o.status] >= 0 && rank[o.status] < 3)
  const rests = useRestaurants({})
  const now = useNow(1000)
  const handoff = useHandoff()
  const cancel = useMutation({ mutationFn: () => api.cancelOrder(orderId), onSuccess: (x) => upsertOrder(qc, x) })
  const [celebrate, setCelebrate] = useState(false)
  const [receiptOpen, setReceiptOpen] = useState(false)
  const prevStatus = useRef<OrderStatus | undefined>(undefined)

  // Celebrate only a delivery that happens while you're watching.
  useEffect(() => {
    if (!o) return
    const was = prevStatus.current
    prevStatus.current = o.status
    if (was && rank[was] < 3 && rank[o.status] === 3) setCelebrate(true)
    if (was === 'purchased' && o.status === 'picked_up') pop(document.getElementById('map-chip'), { label: `${o.runner.display_name} picked up your food`, size: 64 })
  }, [o])

  useEffect(() => {
    if (o) document.title = `Your ${o.runner.display_name} ferry`
  }, [o])

  const restaurant = rests.data?.find((r) => r.id === o?.restaurant_id) ?? run.data?.restaurant
  const runnerLoc = track.data?.runner_location ?? run.data?.runner_location ?? null
  const eta = track.data?.eta_minutes ?? run.data?.eta_minutes ?? o?.eta_minutes ?? null
  const dist = track.data?.distance_m ?? null
  const age = runnerLoc ? now - Date.parse(runnerLoc.at) : null
  const stale = age !== null && age > STALE_MS
  const arrived = o?.status === 'picked_up' && ((dist !== null && dist < 40) || eta === 0)

  const markers = useMemo<MapMarker[]>(() => {
    if (!o) return []
    const m: MapMarker[] = [{ id: 'drop', at: o.dropoff.location, kind: 'dropoff', label: o.dropoff.label }]
    if (restaurant && rank[o.status] < 2) m.push({ id: 'rest', at: restaurant.location, kind: 'restaurant', label: restaurant.name })
    if (runnerLoc && rank[o.status] >= 1 && rank[o.status] < 3)
      m.push({ id: 'runner', at: runnerLoc.location, kind: 'runner', color: o.runner.bubble_color, initial: o.runner.display_name.charAt(0), stale })
    return m
  }, [o, restaurant, runnerLoc, stale])

  const fit = useMemo(() => {
    if (!o) return []
    if (runnerLoc && o.status === 'picked_up') return [runnerLoc.location, o.dropoff.location]
    return [o.dropoff.location, ...(restaurant ? [restaurant.location] : [])]
    // Refit only when the phase changes, not on every position update.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [o?.status, restaurant?.id, !!runnerLoc])

  const liveText = useThrottled(
    o?.status === 'picked_up' && eta !== null ? `${o.runner.display_name} is ${eta} minutes away${dist !== null ? `, ${miles(dist)}` : ''}.` : '',
    15000,
  )

  if (order.isPending)
    return (
      <>
        <TopBar title="Your order" back="/orders" />
        <Page>
          <Loading label="Loading your order" rows={4} />
        </Page>
      </>
    )
  if (order.isError || !o)
    return (
      <>
        <TopBar title="Your order" back="/orders" />
        <Page>
          <Panel>
            <ErrorState error={order.error} onRetry={() => order.refetch()} title="We couldn’t open this order" />
          </Panel>
        </Page>
      </>
    )

  const r = o.runner.display_name
  const tone = rank[o.status] === 3 ? 'done' : moving ? 'moving' : 'waiting'
  const isCustomer = me.data?.id === o.customer.id
  const headline = (() => {
    switch (o.status) {
      case 'funded':
        return { h: 'Order placed', p: `${r} buys it when the window closes, then walks it over.` }
      case 'purchased':
        return { h: `${r} bought your food`, p: 'Waiting for it to be ready, then the walk starts.' }
      case 'picked_up':
        return arrived ? { h: `${r} is here`, p: `Meet at ${o.dropoff.label} and show your code.` } : { h: `${r} is bringing your food`, p: `Walking it to ${o.dropoff.label}.` }
      case 'delivered':
      case 'settled':
        return { h: 'Delivered', p: o.co2_saved_g ? `This walk saved ${grams(o.co2_saved_g)} of CO2 vs a car delivery.` : 'Enjoy.' }
      case 'cancelled':
        return { h: 'Order cancelled', p: 'Nothing was charged.' }
      case 'refunded':
        return { h: 'Order refunded', p: 'The hold went back to your Ferry Wallet.' }
    }
  })()

  return (
    <>
      <TopBar title={restaurant ? `${restaurant.name} with ${r}` : 'Your order'} back="/orders" />
      <div className={s.layout}>
        <div className={s.mapPane}>
          <LazyMap
            label={`Map showing ${r}${runnerLoc ? '’s position' : ''} and your drop-off at ${o.dropoff.label}`}
            center={o.dropoff.location}
            zoom={15}
            markers={markers}
            route={track.data?.route ?? null}
            fit={fit}
          />
          {o.status === 'picked_up' && (
            <div id="map-chip" className={`${s.mapChip} ${glassClass}`}>
              <span className={`${s.eta} num`}>{eta !== null ? (eta === 0 ? 'Here' : `${eta} min`) : '...'}</span>
              <span className={s.chipText}>
                <strong>{dist !== null ? `${miles(dist)} away` : `${r} is walking`}</strong>
                <span>{runnerLoc ? (stale ? `Last seen ${ago(runnerLoc.at, now)}` : `Seen ${ago(runnerLoc.at, now)}`) : 'Waiting for location'}</span>
              </span>
            </div>
          )}
        </div>

        <div className={s.content}>
          <p className="sr-only" aria-live="polite">
            {liveText}
          </p>

          {justPlaced && o.status === 'funded' && (
            <Panel className={s.placed}>
              <h2 style={{ fontSize: 'var(--fs-lg)' }}>Order confirmed</h2>
              <p>
                <strong className="num">{money(o.total_cents)}</strong> is on hold in your Ferry Wallet. Anything the receipt comes in under comes back to you.
              </p>
            </Panel>
          )}

          <Panel className={s.status} data-tone={tone}>
            <h1>{headline.h}</h1>
            <p className="muted">{headline.p}</p>
            {rank[o.status] >= 0 && rank[o.status] < 3 && (
              <p className={s.who}>
                <Avatar user={o.runner} size={32} label={false} />
                <span>
                  {r} carries your food. Ferry just tracks the walk and holds your {money(o.total_cents)} until it’s in your hands.
                </span>
              </p>
            )}
            {rank[o.status] >= 0 && <Timeline order={o} />}
          </Panel>

          {o.status === 'picked_up' && o.handoff_code && (
            <Panel className={s.code}>
              <h2 style={{ fontSize: 'var(--fs-md)' }}>Your handoff code</h2>
              <div className={s.codeDigits} aria-label={`Handoff code ${o.handoff_code.split('').join(' ')}`} role="img">
                {o.handoff_code.split('').map((d, i) => (
                  <span key={i} className="num" aria-hidden>
                    {d}
                  </span>
                ))}
              </div>
              <p className="muted">Show this to {r}, or tap Got it when the food is in your hands.</p>
              <GelButton
                size="lg"
                variant="eco"
                block
                busy={handoff.isPending}
                onClick={() => handoff.mutate({ orderId: o.id })}
              >
                Got it
              </GelButton>
              {handoff.isError && (
                <p className={c.error} role="alert">
                  {handoff.error instanceof ApiError ? handoff.error.message : 'That didn’t go through. Try again.'}
                </p>
              )}
            </Panel>
          )}

          {o.receipt_url && (
            <Panel pad="tight" className={s.receipt}>
              <button type="button" className={s.receiptThumb} onClick={() => setReceiptOpen(true)} aria-label="Open the receipt photo">
                <img src={o.receipt_url} alt="" loading="lazy" decoding="async" onError={(e) => (e.currentTarget.style.display = 'none')} />
              </button>
              <div style={{ display: 'grid', gap: 2 }}>
                <strong style={{ fontWeight: 600, display: 'flex', alignItems: 'center', gap: 6 }}>
                  <Receipt size={18} aria-hidden /> Receipt from {r}
                </strong>
                <span className="muted" style={{ fontSize: 'var(--fs-sm)' }}>
                  Bought with the Ferry Card. Your share is settled from this receipt.
                </span>
              </div>
            </Panel>
          )}

          <Panel>
            <h2 style={{ fontSize: 'var(--fs-md)', marginBottom: 12 }}>Your order</h2>
            <ul className={s.lines}>
              {o.lines.map((l, i) => (
                <li key={i}>
                  <span>
                    {l.qty} × {l.name}
                    {l.note && <span className="muted"> “{l.note}”</span>}
                  </span>
                  <span className="num">{money(l.line_total_cents)}</span>
                </li>
              ))}
            </ul>
            <dl className={s.totals}>
              <div>
                <dt>Subtotal</dt>
                <dd className="num">{money(o.subtotal_cents)}</dd>
              </div>
              <div>
                <dt>Tax</dt>
                <dd className="num">{money(o.tax_cents)}</dd>
              </div>
              <div>
                <dt>Ferry fee</dt>
                <dd className="num">{money(o.ferry_fee_cents)}</dd>
              </div>
              <div>
                <dt>Tip for {r}</dt>
                <dd className="num">{money(o.tip_cents)}</dd>
              </div>
              <div className={s.grand}>
                <dt>Total</dt>
                <dd className="num">{money(o.total_cents)}</dd>
              </div>
            </dl>
            <p className={c.note} style={{ marginTop: 8 }}>
              Paid from your Ferry Wallet. Sandbox money (Capital One Nessie).
            </p>
            {o.status === 'funded' && isCustomer && (
              <div style={{ marginTop: 12 }}>
                <GelButton variant="danger" size="sm" busy={cancel.isPending} onClick={() => cancel.mutate()}>
                  Cancel order
                </GelButton>
                {cancel.isError && (
                  <p className={c.error} role="alert">
                    {cancel.error instanceof ApiError ? cancel.error.message : 'Couldn’t cancel.'}
                  </p>
                )}
              </div>
            )}
          </Panel>
        </div>
      </div>

      <Sheet open={receiptOpen} onClose={() => setReceiptOpen(false)} title={`Receipt from ${r}`}>
        {o.receipt_url && <img className={s.receiptFull} src={o.receipt_url} alt={`Photo of the receipt ${r} uploaded`} />}
      </Sheet>
      <Celebration
        open={celebrate}
        grams={o.co2_saved_g ?? 480}
        onClose={() => {
          setCelebrate(false)
          qc.invalidateQueries({ queryKey: keys.wallet })
        }}
      />
    </>
  )
}
