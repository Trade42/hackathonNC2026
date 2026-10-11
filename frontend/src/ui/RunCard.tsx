import { Link } from 'react-router-dom'
import type { Run } from '../api/types'
import { clock, countdown, plural } from '../lib/format'
import { useNow } from '../lib/time'
import c from '../screens/common.module.css'
import { Avatar, FoodBubble } from './Bubble'
import { CountdownRing } from './CountdownRing'
import { Panel } from './Surface'

// An open ferry: the ring around the restaurant bubble drains as the window closes.
export function RunCard({ run, to }: { run: Run; to?: string }) {
  const now = useNow(1000)
  const left = Date.parse(run.window_closes_at) - now
  const urgent = left < 2 * 60e3
  return (
    <Panel as="li" pad="flush">
      <Link to={to ?? `/r/${run.id}`} className={c.row} aria-label={`${run.runner.display_name} is going to ${run.restaurant.name}. ${left > 0 ? `${countdown(left)} left to order` : 'Window closed'}.`}>
        <span style={{ position: 'relative' }}>
          <CountdownRing closesAt={run.window_closes_at} size={68} label={run.restaurant.name}>
            <FoodBubble size={52} cuisine={run.restaurant.cuisine} imageUrl={run.restaurant.image_url} name={run.restaurant.name} />
          </CountdownRing>
          <span style={{ position: 'absolute', right: -4, bottom: -2 }}>
            <Avatar user={run.runner} size={26} label={false} />
          </span>
        </span>
        <span className={c.rowMain}>
          <h3>{run.restaurant.name}</h3>
          <p>
            {run.runner.display_name} leaves at {clock(run.leaving_at)}. {plural(run.order_count, 'order')} in.
          </p>
        </span>
        <span className={c.rowAside}>
          <span className={`${c.pill} ${urgent ? c.pillWarn : ''} num`}>{left > 0 ? countdown(left) : 'Closed'}</span>
          <span className="muted" style={{ fontSize: 'var(--fs-xs)' }}>
            to order
          </span>
        </span>
      </Link>
    </Panel>
  )
}
