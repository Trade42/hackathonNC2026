import type { Restaurant } from '../api/types'
import { cuisineLabel, hoursLabel } from '../lib/format'
import { CuisineArt } from '../ui/CuisineArt'
import s from './Order.module.css'

// The restaurant banner: its art (or photo), name and the real facts as pills.
export function RestaurantHeader({ r }: { r: Restaurant }) {
  return (
    <div className={s.header}>
      {r.image_url ? <img className={s.banner} src={r.image_url} alt="" /> : <CuisineArt cuisine={r.cuisine} variant="banner" className={s.banner} />}
      <div className={s.headerBody}>
        <h1>{r.name}</h1>
        <ul className={s.facts} aria-label="About this restaurant">
          <li>{cuisineLabel(r.cuisine.slice(0, 2))}</li>
          <li aria-label={`price level ${r.price_level} of 3`}>{'$'.repeat(r.price_level)}</li>
          {r.walk_minutes_from_dropoff != null && <li>{r.walk_minutes_from_dropoff} min walk</li>}
          <li className={r.is_open ? s.factOpen : s.factClosed}>{r.is_open ? `Open ${hoursLabel(r.hours_today)}` : 'Closed now'}</li>
        </ul>
        <p className="muted">{r.address}</p>
      </div>
    </div>
  )
}
