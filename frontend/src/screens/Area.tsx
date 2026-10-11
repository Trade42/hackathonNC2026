import { MagnifyingGlass, Storefront } from '@phosphor-icons/react'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router-dom'
import { useAreas, useMe, useOpenRuns, useRestaurants } from '../api/hooks'
import { cuisineLabel } from '../lib/format'
import { sortNearest, useNearest } from '../lib/nearest'
import { Page, TopBar } from '../shell/TopBar'
import { FoodBubble } from '../ui/Bubble'
import { Chip, chipGroupClass } from '../ui/Chip'
import { Empty, ErrorState, Skeleton } from '../ui/States'
import { Panel } from '../ui/Surface'
import c from './common.module.css'
import s from './Area.module.css'

export default function AreaScreen() {
  const { areaId = 'area_franklin' } = useParams()
  const [params, setParams] = useSearchParams()
  const requestMode = params.get('request') === '1'
  const near = params.get('near') === '1'
  const nav = useNavigate()
  const [q, setQ] = useState('')
  const [openNow, setOpenNow] = useState(false)
  const areas = useAreas()
  const rests = useRestaurants({ area_id: areaId, q: q.trim() || undefined, open_now: openNow || undefined })
  const runs = useOpenRuns()
  const me = useMe()
  const origin = useNearest(near, me.data?.profile.dropoff_default?.location)
  const area = areas.data?.find((a) => a.id === areaId)
  const list = sortNearest(rests.data ?? [], origin)

  useEffect(() => {
    document.title = `${area?.name ?? 'Food'} on Ferry`
  }, [area])

  const runFor = (rid: string) => runs.data?.find((r) => r.restaurant.id === rid)
  const suffix = requestMode ? '?request=1' : ''
  const toggleNear = () =>
    setParams(
      (p) => {
        const n = new URLSearchParams(p)
        if (near) n.delete('near')
        else n.set('near', '1')
        return n
      },
      { replace: true },
    )

  return (
    <>
      <TopBar title={area?.name ?? 'Order food'} back="/home" />
      <Page>
        {requestMode && (
          <p className={s.banner}>Pick where you want food from. Ferry looks for a student already heading there.</p>
        )}
        <div className={chipGroupClass} role="radiogroup" aria-label="Area" style={{ marginBottom: 16 }}>
          {areas.data?.map((a) => (
            <Chip key={a.id} role="radio" selected={a.id === areaId} onClick={() => nav({ pathname: `/food/${a.id}`, search: params.toString() }, { replace: true })}>
              {a.name}
            </Chip>
          ))}
        </div>
        <div className={s.filters}>
          <label className={s.search}>
            <MagnifyingGlass size={20} aria-hidden />
            <span className="sr-only">Search restaurants</span>
            <input type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder="Search by name or cuisine" enterKeyHint="search" />
          </label>
          <Chip selected={openNow} onClick={() => setOpenNow(!openNow)}>
            Open now
          </Chip>
          <Chip selected={near} onClick={toggleNear}>
            Nearest
          </Chip>
        </div>

        {rests.isPending ? (
          <div className="stack" aria-hidden>
            {[0, 1, 2, 3].map((i) => (
              <Skeleton key={i} h={84} />
            ))}
          </div>
        ) : rests.isError ? (
          <Panel>
            <ErrorState error={rests.error} onRetry={() => rests.refetch()} />
          </Panel>
        ) : list.length === 0 ? (
          <Panel>
            <Empty icon={<Storefront size={28} color="var(--sky-700)" aria-hidden />} title="Nothing matches">
              {openNow ? 'Nothing here is open right now. Turn off “Open now” to see everything.' : 'Try another search or area.'}
            </Empty>
          </Panel>
        ) : (
          <ul className={c.list}>
            {list.map((r) => {
              const run = runFor(r.id)
              return (
                <Panel as="li" pad="flush" key={r.id}>
                  <Link to={`/food/${areaId}/${r.id}${suffix}`} className={c.row}>
                    <FoodBubble size={56} cuisine={r.cuisine} imageUrl={r.image_url} name={r.name} />
                    <span className={c.rowMain}>
                      <h3>{r.name}</h3>
                      <p>
                        {cuisineLabel(r.cuisine)}
                        <span aria-label={`price level ${r.price_level} of 3`}> {'$'.repeat(r.price_level)}</span>
                        {r.walk_minutes_from_dropoff != null && `. ${r.walk_minutes_from_dropoff} min walk`}
                      </p>
                      {run && <p className={s.going}>{run.runner.display_name} is going now</p>}
                    </span>
                    <span className={c.rowAside}>
                      <span className={`${c.pill} ${r.is_open ? '' : c.pillMuted}`}>{r.is_open ? 'Open' : 'Closed'}</span>
                    </span>
                  </Link>
                </Panel>
              )
            })}
          </ul>
        )}
      </Page>
    </>
  )
}
