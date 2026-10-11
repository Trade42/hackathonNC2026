import { Link } from 'react-router-dom'
import type { Restaurant, Run } from '../api/types'
import { countdown, cuisineLabel, hoursLabel } from '../lib/format'
import { useNow } from '../lib/time'
import { Avatar } from './Bubble'
import { CuisineArt } from './CuisineArt'
import f from './Feed.module.css'

const price = (n: number) => '$'.repeat(n)

// A restaurant in the feed: art, name, then only real facts (cuisine, price
// level, walk time, today's hours), plus who's going there right now.
export function RestaurantCard({ r, run, to }: { r: Restaurant; run?: Run; to?: string }) {
  const now = useNow(1000)
  const left = run ? Date.parse(run.window_closes_at) - now : 0
  const facts = [cuisineLabel(r.cuisine.slice(0, 2)), price(r.price_level), r.walk_minutes_from_dropoff != null ? `${r.walk_minutes_from_dropoff} min walk` : null].filter(Boolean).join(', ')
  return (
    <li className={f.restaurant}>
      <Link to={to ?? `/food/${r.area_id}/${r.id}`} className={f.restLink}>
        {r.image_url ? <img className={f.restImg} src={r.image_url} alt="" loading="lazy" /> : <CuisineArt cuisine={r.cuisine} />}
        <span className={f.restMain}>
          <h3>{r.name}</h3>
          <span className={f.meta}>{facts}</span>
          <span className={r.is_open ? f.open : f.closed}>{r.is_open ? `Open ${hoursLabel(r.hours_today)}` : 'Closed now'}</span>
          {run && left > 0 && (
            <span className={f.going}>
              <Avatar user={run.runner} size={22} label={false} />
              <span>
                {run.runner.display_name} is going, <span className="num">{countdown(left)}</span> left
              </span>
            </span>
          )}
        </span>
      </Link>
    </li>
  )
}
