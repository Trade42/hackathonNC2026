import { Link } from 'react-router-dom'
import type { Order } from '../api/types'
import { orderHeadline, stepIndex } from '../lib/orders'
import { Avatar } from './Bubble'
import f from './Feed.module.css'

const LABELS = ['Placed', 'Bought', 'On the way', 'Here']

// The order in progress, pinned at the top of Home: who is bringing it, the
// ETA, and the four delivery steps.
export function LiveOrderCard({ order, restaurant }: { order: Order; restaurant?: string }) {
  const step = stepIndex(order.status)
  const items = order.lines.map((l) => `${l.qty > 1 ? `${l.qty} ` : ''}${l.name}`).join(', ')
  return (
    <Link to={`/o/${order.id}`} className={f.live} aria-label={`${orderHeadline(order, restaurant)}. Track your order.`}>
      <span className={f.liveTop}>
        <Avatar user={order.runner} size={44} label={false} />
        <span className={f.liveText}>
          <strong>{orderHeadline(order, restaurant)}</strong>
          <small>
            {items}, to {order.dropoff.label}
          </small>
        </span>
        {order.status === 'picked_up' && order.eta_minutes != null && (
          <span className={`${f.eta} num`}>
            {order.eta_minutes}
            <small>min</small>
          </span>
        )}
      </span>
      <span className={f.steps} aria-hidden>
        {LABELS.map((l, i) => (
          <span key={l} data-on={i <= step} data-now={i === step + 1 && order.status !== 'delivered'} />
        ))}
      </span>
      <span className={f.stepLabels} aria-hidden>
        {LABELS.map((l) => (
          <span key={l}>{l}</span>
        ))}
      </span>
    </Link>
  )
}
