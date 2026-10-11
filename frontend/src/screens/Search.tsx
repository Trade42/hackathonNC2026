import { MagnifyingGlass, Storefront } from '@phosphor-icons/react'
import { useEffect, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { useMe, useOpenRuns, useRestaurants } from '../api/hooks'
import { plural } from '../lib/format'
import { sortNearest, useNearest } from '../lib/nearest'
import { Page, TopBar } from '../shell/TopBar'
import { Chip, chipGroupClass } from '../ui/Chip'
import { cuisineFamily, type Family } from '../ui/CuisineArt'
import { CUISINES } from '../ui/cuisines'
import f from '../ui/Feed.module.css'
import { RestaurantCard } from '../ui/RestaurantCard'
import { Empty, ErrorState, Skeleton } from '../ui/States'
import { Panel } from '../ui/Surface'
import s from './Search.module.css'

// Find a restaurant by name or cuisine. Filters live in the URL so the back
// button and a reload keep them.
export default function Search() {
  const [params, setParams] = useSearchParams()
  const q = params.get('q') ?? ''
  const cuisine = params.get('cuisine') as Family | null
  const openNow = params.get('open') === '1'
  const near = params.get('near') === '1'
  // "Request a ferry" starts here: pick the place, then the menu, then where it goes.
  const requestMode = params.get('request') === '1'
  const [text, setText] = useState(q)
  const rests = useRestaurants({ q: q || undefined, open_now: openNow || undefined })
  const runs = useOpenRuns()
  const me = useMe()
  const origin = useNearest(near, me.data?.profile.dropoff_default?.location)

  useEffect(() => {
    document.title = requestMode ? 'Request a ferry' : 'Search on Ferry'
  }, [requestMode])

  // Debounce typing into the URL (and the query).
  useEffect(() => {
    const t = window.setTimeout(() => {
      if (text.trim() === q) return
      setParams(
        (p) => {
          const n = new URLSearchParams(p)
          if (text.trim()) n.set('q', text.trim())
          else n.delete('q')
          return n
        },
        { replace: true },
      )
    }, 250)
    return () => window.clearTimeout(t)
  }, [text, q, setParams])

  const set = (key: string, value: string | null) =>
    setParams(
      (p) => {
        const n = new URLSearchParams(p)
        if (value) n.set(key, value)
        else n.delete(key)
        return n
      },
      { replace: true },
    )

  const list = sortNearest(
    (rests.data ?? []).filter((r) => !cuisine || cuisineFamily(r.cuisine) === cuisine),
    origin,
  )

  return (
    <>
      <TopBar title={requestMode ? 'Request a ferry' : 'Search'} back={requestMode ? '/home' : undefined} />
      <Page>
        <div className="stack">
          {requestMode && (
            <p className={s.intro}>
              Where do you want food from? Ferry offers your request to students heading that way, and the one who takes it walks it to you. If someone’s already going, you can join their run.
            </p>
          )}
          <label className={s.field}>
            <MagnifyingGlass size={20} aria-hidden />
            <input type="search" value={text} onChange={(e) => setText(e.target.value)} placeholder="Search restaurants or cuisines" aria-label="Search restaurants or cuisines" enterKeyHint="search" />
          </label>

          <div className={`${chipGroupClass} ${s.chips}`} role="radiogroup" aria-label="Cuisine">
            <Chip role="radio" selected={!cuisine} onClick={() => set('cuisine', null)}>
              All
            </Chip>
            {CUISINES.map((c) => (
              <Chip key={c.family} role="radio" selected={cuisine === c.family} onClick={() => set('cuisine', cuisine === c.family ? null : c.family)}>
                {c.label}
              </Chip>
            ))}
          </div>
          <div className={chipGroupClass}>
            <Chip selected={openNow} onClick={() => set('open', openNow ? null : '1')}>
              Open now
            </Chip>
            <Chip selected={near} onClick={() => set('near', near ? null : '1')}>
              Nearest
            </Chip>
          </div>

          {rests.isPending ? (
            <div className="stack" aria-hidden>
              <Skeleton h={84} />
              <Skeleton h={84} />
              <Skeleton h={84} />
            </div>
          ) : rests.isError ? (
            <Panel>
              <ErrorState error={rests.error} onRetry={() => rests.refetch()} />
            </Panel>
          ) : list.length === 0 ? (
            <Panel>
              <Empty icon={<Storefront size={28} color="var(--sky-700)" aria-hidden />} title="Nothing matches that">
                Try another name or cuisine, or clear the filters.
              </Empty>
            </Panel>
          ) : (
            <>
              <p className="muted" role="status">
                {plural(list.length, 'place')}
                {origin && (origin.from === 'here' ? ', nearest to you first' : ', nearest your drop-off first')}
              </p>
              <ul className={f.restList}>
                {list.map((r) => {
                  const run = runs.data?.find((x) => x.restaurant.id === r.id)
                  return <RestaurantCard key={r.id} r={r} run={run} to={requestMode ? (run ? `/r/${run.id}` : `/food/${r.area_id}/${r.id}?request=1`) : undefined} />
                })}
              </ul>
            </>
          )}
        </div>
      </Page>
    </>
  )
}
