import { CaretRight } from '@phosphor-icons/react'
import { Link, useLocation } from 'react-router-dom'
import { useMyOrders, useRestaurants } from '../api/hooks'
import { ACTIVE, orderHeadline } from '../lib/orders'
import { Avatar } from '../ui/Bubble'
import { glassClass } from '../ui/Surface'
import s from './Shell.module.css'

// While an order is on its way, a glass chip above the tab bar says who is
// bringing it, on every screen except Home (which has the full card),
// Orders and the tracking screen itself.
export function MiniTracker() {
  const { pathname } = useLocation()
  const orders = useMyOrders()
  const rests = useRestaurants({})
  // Also hidden while ordering, where the cart bar takes that spot.
  const hidden = pathname === '/home' || pathname === '/orders' || pathname.startsWith('/o/') || pathname.startsWith('/r/') || pathname.startsWith('/food/')
  const o = orders.data?.find((x) => ACTIVE.includes(x.status))
  if (hidden || !o) return null
  const restaurant = rests.data?.find((r) => r.id === o.restaurant_id)?.name
  return (
    <Link to={`/o/${o.id}`} className={`${s.mini} ${glassClass}`} aria-label={`${orderHeadline(o, restaurant)}. Track your order.`}>
      <Avatar user={o.runner} size={32} label={false} />
      <span className={s.miniText}>
        <strong>{orderHeadline(o, restaurant)}</strong>
        {o.status === 'picked_up' && o.eta_minutes != null && <small className="num">{o.eta_minutes} min away</small>}
      </span>
      <CaretRight size={18} weight="bold" aria-hidden />
    </Link>
  )
}
