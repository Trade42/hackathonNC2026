import { ArrowCounterClockwise, Receipt } from '@phosphor-icons/react'
import { useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router-dom'
import { useMyOrders, useRestaurants } from '../api/hooks'
import { grams, money, shortDate } from '../lib/format'
import { Page, TopBar } from '../shell/TopBar'
import { FoodBubble } from '../ui/Bubble'
import { refillCart } from '../lib/usual'
import { Chip, chipGroupClass } from '../ui/Chip'
import { GelButton, GelLink } from '../ui/GelButton'
import { LiveOrderCard } from '../ui/LiveOrderCard'
import { Empty, ErrorState, Skeleton } from '../ui/States'
import { Panel } from '../ui/Surface'
import c from './common.module.css'
import { ACTIVE, statusText } from '../lib/orders'

export default function Orders() {
  const orders = useMyOrders()
  const rests = useRestaurants({})
  const rest = (id: string) => rests.data?.find((r) => r.id === id)

  useEffect(() => {
    document.title = 'Your orders on Ferry'
  }, [])

  const qc = useQueryClient()
  const nav = useNavigate()
  const [params, setParams] = useSearchParams()
  const [again, setAgain] = useState<string | null>(null)
  const active = orders.data?.filter((o) => ACTIVE.includes(o.status)) ?? []
  const past = orders.data?.filter((o) => !ACTIVE.includes(o.status)) ?? []
  const tab = params.get('tab') === 'past' || (active.length === 0 && params.get('tab') !== 'now') ? 'past' : 'now'

  // Order again: refill the cart from this order and open the restaurant.
  const orderAgain = async (o: (typeof past)[number]) => {
    const r = rest(o.restaurant_id)
    if (!r) return
    setAgain(o.id)
    try {
      await refillCart(qc, r.id, o)
      nav(`/food/${r.area_id}/${r.id}`)
    } finally {
      setAgain(null)
    }
  }

  const list = (items: typeof active) => (
    <ul className={c.list}>
      {items.map((o) => {
        const r = rest(o.restaurant_id)
        return (
          <Panel as="li" pad="flush" key={o.id}>
            <Link to={`/o/${o.id}`} className={c.row}>
              <FoodBubble size={48} name={r?.name ?? o.restaurant_id} cuisine={r?.cuisine} imageUrl={r?.image_url} />
              <span className={c.rowMain}>
                <h3>{r?.name ?? 'Order'}</h3>
                <p>
                  {o.lines.map((l) => `${l.qty} × ${l.name}`).join(', ')}. With {o.runner.display_name}, {shortDate(o.timeline[0].at)}.
                </p>
              </span>
              <span className={c.rowAside}>
                <span className={`${c.pill} ${ACTIVE.includes(o.status) ? '' : c.pillMuted}`}>{statusText[o.status]}</span>
                <span className="num">{money(o.total_cents)}</span>
                {o.co2_saved_g ? <span className={`num co2`} style={{ fontSize: 'var(--fs-xs)', fontWeight: 600 }}>+{grams(o.co2_saved_g)} CO2</span> : null}
              </span>
            </Link>
            {r && o.status !== 'cancelled' && (
              <div className={c.rowFoot}>
                <GelButton size="sm" variant="white" busy={again === o.id} icon={<ArrowCounterClockwise size={18} aria-hidden />} onClick={() => orderAgain(o)} aria-label={`Order again from ${r.name}`}>
                  Order again
                </GelButton>
              </div>
            )}
          </Panel>
        )
      })}
    </ul>
  )

  return (
    <>
      <TopBar title="Orders" />
      <Page>
        {orders.isPending ? (
          <div className="stack" aria-hidden>
            <Skeleton h={84} />
            <Skeleton h={84} />
          </div>
        ) : orders.isError ? (
          <Panel>
            <ErrorState error={orders.error} onRetry={() => orders.refetch()} />
          </Panel>
        ) : orders.data.length === 0 ? (
          <Panel>
            <Empty icon={<Receipt size={28} color="var(--sky-700)" aria-hidden />} title="No orders yet" action={<GelLink to="/home">See ferries leaving soon</GelLink>}>
              When a student heads out, add your order. They’ll bring it, and it shows up here.
            </Empty>
          </Panel>
        ) : (
          <div className="stack">
            <div className={chipGroupClass} role="radiogroup" aria-label="Which orders">
              <Chip role="radio" selected={tab === 'now'} onClick={() => setParams({ tab: 'now' }, { replace: true })}>
                In progress ({active.length})
              </Chip>
              <Chip role="radio" selected={tab === 'past'} onClick={() => setParams({ tab: 'past' }, { replace: true })}>
                Past ({past.length})
              </Chip>
            </div>
            {tab === 'now' ? (
              active.length ? (
                <div className="stack">
                  {active.map((o) => (
                    <LiveOrderCard key={o.id} order={o} restaurant={rest(o.restaurant_id)?.name} />
                  ))}
                </div>
              ) : (
                <Panel>
                  <Empty icon={<Receipt size={28} color="var(--sky-700)" aria-hidden />} title="Nothing on the way" action={<GelLink to="/home">See who’s heading out</GelLink>}>
                    When a captain is bringing your food, you can follow them here.
                  </Empty>
                </Panel>
              )
            ) : past.length ? (
              list(past)
            ) : (
              <Panel>
                <Empty icon={<Receipt size={28} color="var(--sky-700)" aria-hidden />} title="No past orders yet">
                  Orders you’ve finished show up here, ready to order again.
                </Empty>
              </Panel>
            )}
          </div>
        )}
      </Page>
    </>
  )
}
