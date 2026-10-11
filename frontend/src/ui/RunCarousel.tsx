import { Link } from 'react-router-dom'
import type { Run } from '../api/types'
import { clock, countdown, plural } from '../lib/format'
import { useNow } from '../lib/time'
import { Avatar } from './Bubble'
import { CuisineArt } from './CuisineArt'
import f from './Feed.module.css'

// Friends heading out, as swipeable cards: where, who's walking, and how long
// is left to add an order.
export function RunCarousel({ runs }: { runs: Run[] }) {
  const now = useNow(1000)
  return (
    <ul className={f.carousel} aria-label="Ferries leaving soon">
      {runs.map((run) => {
        const left = Date.parse(run.window_closes_at) - now
        const who = run.runner.display_name
        return (
          <li key={run.id} className={f.runItem}>
            <Link to={`/r/${run.id}`} className={f.runLink} aria-label={`${who} is going to ${run.restaurant.name}. ${left > 0 ? `${countdown(left)} left to order` : 'Window closed'}.`}>
              <span className={f.runArt}>
                {run.restaurant.image_url ? (
                  <img src={run.restaurant.image_url} alt="" style={{ width: '100%', aspectRatio: '16 / 9', objectFit: 'cover' }} />
                ) : (
                  <CuisineArt cuisine={run.restaurant.cuisine} variant="card" />
                )}
                <span className={`${f.chip} ${left < 2 * 60e3 ? f.chipWarn : ''} num`}>{left > 0 ? `${countdown(left)} left` : 'Closed'}</span>
              </span>
              <span className={f.runBody}>
                <Avatar user={run.runner} size={36} label={false} />
                <span className={f.runText}>
                  <h3>{run.restaurant.name}</h3>
                  <p>
                    {who} leaves at {clock(run.leaving_at)}
                    {run.returning_at ? `, back around ${clock(run.returning_at)}` : ''}
                  </p>
                  {run.return_place && (
                    <p className={f.plan}>
                      To {run.return_place.label}
                      {run.dropoff_radius_m ? `, drops off within ${run.dropoff_radius_m} m` : ''}
                    </p>
                  )}
                  <p className={f.count}>{plural(run.order_count, 'order')} in</p>
                </span>
              </span>
            </Link>
          </li>
        )
      })}
    </ul>
  )
}
