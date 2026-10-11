import { Basket } from '@phosphor-icons/react'
import { Link } from 'react-router-dom'
import { money } from '../lib/format'
import { useCart } from '../state/cart'
import c from '../screens/common.module.css'
import s from './Order.module.css'

export function CartBar({ cartKey, to, label = 'Review order' }: { cartKey: string; to: string; label?: string }) {
  const { count, subtotal } = useCart(cartKey)
  if (!count) return null
  return (
    <div className={c.actionBar}>
      <Link to={to} id="cart-bar" className={s.cartBar}>
        <span className={`${s.cartCount} num`} aria-hidden>
          {count}
        </span>
        <Basket size={20} weight="fill" aria-hidden />
        <span className={s.cartLabel}>
          {label}
          <span className="sr-only">, {count} {count === 1 ? 'item' : 'items'}</span>
        </span>
        <span className={`${s.cartTotal} num`}>{money(subtotal)}</span>
      </Link>
    </div>
  )
}
