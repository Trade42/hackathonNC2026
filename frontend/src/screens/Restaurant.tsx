import { useEffect } from 'react'
import { Link, useParams, useSearchParams } from 'react-router-dom'
import { useMenu, useOpenRuns, useRestaurants } from '../api/hooks'
import { CartBar } from '../order/CartBar'
import { Menu } from '../order/Menu'
import { RestaurantHeader } from '../order/RestaurantHeader'
import { Page, TopBar } from '../shell/TopBar'
import type { Run } from '../api/types'
import { clock, countdown } from '../lib/format'
import { useNow } from '../lib/time'
import o from '../order/Order.module.css'
import { Avatar } from '../ui/Bubble'
import { ErrorState, Loading } from '../ui/States'
import { Panel } from '../ui/Surface'
import c from './common.module.css'

// Who's going there now, and how long is left to add an order.
function RunBanner({ run }: { run: Run }) {
  const now = useNow(1000)
  const left = Date.parse(run.window_closes_at) - now
  return (
    <Link to={`/r/${run.id}`} className={o.runBanner} aria-label={`${run.runner.display_name} is going. ${left > 0 ? `${countdown(left)} left to order` : 'Window closed'}.`}>
      <Avatar user={run.runner} size={44} label={false} />
      <span className={o.runText}>
        <strong>{run.runner.display_name} is going and will bring your food</strong>
        <small>Leaves at {clock(run.leaving_at)}. Order in the next</small>
      </span>
      <span className={`${o.runLeft} num`}>{left > 0 ? countdown(left) : 'Closed'}</span>
    </Link>
  )
}

export default function RestaurantScreen() {
  const { areaId = '', restaurantId = '' } = useParams()
  const [params] = useSearchParams()
  const rests = useRestaurants({ area_id: areaId })
  const menu = useMenu(restaurantId)
  const runs = useOpenRuns()
  const r = rests.data?.find((x) => x.id === restaurantId)
  const run = params.get('request') === '1' ? undefined : runs.data?.find((x) => x.restaurant.id === restaurantId)

  useEffect(() => {
    if (r) document.title = `${r.name} on Ferry`
  }, [r])

  return (
    <>
      <TopBar title={r?.name ?? 'Menu'} back={`/food/${areaId}`} />
      <Page wide>
        <div className={c.split}>
          <div className={c.sticky}>
            <div className="stack">
              {r ? <RestaurantHeader r={r} /> : rests.isError ? <ErrorState error={rests.error} onRetry={() => rests.refetch()} /> : <Loading rows={1} />}
              {run ? (
                <RunBanner run={run} />
              ) : (
                <Panel pad="tight">
                  <p>
                    <strong>No ferry is heading here right now.</strong>
                  </p>
                  <p className="muted">Build your order and Ferry looks for a nearby student heading out.</p>
                </Panel>
              )}
            </div>
          </div>
          <div>
            {menu.isPending ? (
              <Loading label="Loading the menu" rows={5} />
            ) : menu.isError ? (
              <Panel>
                <ErrorState error={menu.error} onRetry={() => menu.refetch()} />
              </Panel>
            ) : (
              <Menu menu={menu.data} cartKey={restaurantId} />
            )}
            <CartBar
              cartKey={restaurantId}
              to={run ? `/r/${run.id}/checkout` : `/food/${areaId}/${restaurantId}/request`}
              label={run ? `Order with ${run.runner.display_name}` : 'Request a ferry'}
            />
          </div>
        </div>
      </Page>
    </>
  )
}
