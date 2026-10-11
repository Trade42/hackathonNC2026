import { Crosshair, MagnifyingGlass, MapPin, NavigationArrow } from '@phosphor-icons/react'
import { useEffect, useId, useRef, useState } from 'react'
import type { LatLng, Place } from '../api/types'
import { currentLocation, placeAt, searchPlaces, walkMinutes } from '../lib/geocode'
import { CAMPUS_CENTER } from '../lib/places'
import { LazyMap } from '../map/LazyMap'
import { AeroIcon } from './AeroIcon'
import { GelButton } from './GelButton'
import s from './LocationPicker.module.css'
import { Sheet } from './Sheet'

export type PlaceZone = { place: Place; radius_m: number }

const describe = (m: number) => `${m < 1000 ? `${m} m` : `${(m / 1000).toFixed(1)} km`}, about ${walkMinutes(m)} min walk`

// Pick a spot and a zone around it: search the map, use your location, tap a
// saved spot, or drag the map under the pin. Used for where food should go
// (requests and orders) and where a runner heads back to (going somewhere).
export function LocationPicker({
  value,
  onChange,
  heading,
  zoneLabel,
  sheetTitle,
  sheetSubtitle,
  saved = [],
  minRadius = 50,
  maxRadius = 1000,
  step = 50,
}: {
  value: PlaceZone
  onChange: (v: PlaceZone) => void
  heading: string
  zoneLabel: string
  sheetTitle: string
  sheetSubtitle?: string
  saved?: Place[]
  minRadius?: number
  maxRadius?: number
  step?: number
}) {
  const [open, setOpen] = useState(false)
  const [draft, setDraft] = useState<PlaceZone>(value)
  const [q, setQ] = useState('')
  const [results, setResults] = useState<Place[]>([])
  const [searching, setSearching] = useState(false)
  const [locating, setLocating] = useState(false)
  const [locError, setLocError] = useState('')
  const [focus, setFocus] = useState<LatLng>(value.place.location)
  const skipReverse = useRef(false)
  const sliderId = useId()

  useEffect(() => {
    if (open) {
      setDraft(value)
      setFocus(value.place.location)
      setQ('')
      setResults([])
      setLocError('')
    }
    // Reset the draft each time the sheet opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open])

  // Debounced search.
  useEffect(() => {
    if (q.trim().length < 2) {
      setResults([])
      return
    }
    let alive = true
    setSearching(true)
    const t = window.setTimeout(async () => {
      const found = await searchPlaces(q, draft.place.location, saved)
      if (alive) {
        setResults(found)
        setSearching(false)
      }
    }, 300)
    return () => {
      alive = false
      window.clearTimeout(t)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [q])

  const choose = (p: Place) => {
    skipReverse.current = true
    setDraft((d) => ({ ...d, place: p }))
    setFocus(p.location)
    setResults([])
    setQ('')
  }

  const useMine = async () => {
    setLocating(true)
    setLocError('')
    try {
      const at = await currentLocation()
      const p = await placeAt(at)
      choose({ ...p, label: p.label === 'Dropped pin' ? 'Your location' : p.label })
    } catch {
      setLocError('Location is off for Ferry. Search or drag the map instead.')
    } finally {
      setLocating(false)
    }
  }

  // Dragging the map moves the pin; name the new spot from the map.
  const onCenter = async (at: LatLng) => {
    if (skipReverse.current) {
      skipReverse.current = false
      return
    }
    setDraft((d) => ({ ...d, place: { label: 'Finding the spot…', location: at } }))
    const p = await placeAt(at)
    setDraft((d) => (d.place.location === at ? { ...d, place: p } : d))
  }

  return (
    <>
      <button type="button" className={s.summary} onClick={() => setOpen(true)} aria-label={`${heading}: ${value.place.label}, ${zoneLabel.toLowerCase()} ${describe(value.radius_m)}. Change`}>
        <AeroIcon icon={MapPin} tone="sky" shape="orb" size={44} />
        <span className={s.summaryText}>
          <small>{heading}</small>
          <strong>{value.place.label}</strong>
          <span>
            {zoneLabel} {describe(value.radius_m)}
          </span>
        </span>
        <span className={s.change}>Change</span>
      </button>

      <Sheet
        open={open}
        onClose={() => setOpen(false)}
        title={sheetTitle}
        subtitle={sheetSubtitle}
        footer={
          <GelButton
            block
            size="lg"
            onClick={() => {
              onChange(draft)
              setOpen(false)
            }}
          >
            Use this spot
          </GelButton>
        }
      >
        <div className={s.body}>
          <label className={s.search}>
            <MagnifyingGlass size={20} aria-hidden />
            <input type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder="Search a building or address" aria-label="Search a building or address" enterKeyHint="search" />
          </label>
          {q.trim().length >= 2 && (
            <ul className={s.results} aria-label="Places" aria-busy={searching}>
              {results.map((p, i) => (
                <li key={`${p.label}-${i}`}>
                  <button type="button" onClick={() => choose(p)}>
                    <MapPin size={18} aria-hidden />
                    {p.label}
                  </button>
                </li>
              ))}
              {!searching && results.length === 0 && <li className={s.none}>No matches nearby. Try a building name.</li>}
            </ul>
          )}

          <div className={s.quick}>
            <button type="button" className={s.chip} onClick={useMine} aria-busy={locating}>
              <NavigationArrow size={16} weight="fill" aria-hidden />
              {locating ? 'Finding you…' : 'Use my location'}
            </button>
            {saved.map((p) => (
              <button key={p.id ?? p.label} type="button" className={s.chip} onClick={() => choose(p)}>
                {p.label}
              </button>
            ))}
          </div>
          {locError && (
            <p className={s.error} role="alert">
              {locError}
            </p>
          )}

          <div className={s.mapBox}>
            <LazyMap
              label="Map. Drag it to move the pin."
              center={draft.place.location ?? CAMPUS_CENTER}
              zoom={16}
              fit={[focus]}
              zoneRadius_m={draft.radius_m}
              onCenterChange={onCenter}
            />
            <span className={s.pin} aria-hidden>
              <MapPin size={36} weight="fill" />
            </span>
            <span className={s.hint} aria-hidden>
              <Crosshair size={14} /> Drag the map to move the pin
            </span>
          </div>
          <p className={s.spot} aria-live="polite">
            <strong>{draft.place.label}</strong>
          </p>

          <div className={s.slider}>
            <label htmlFor={sliderId}>
              {zoneLabel} <strong className="num">{describe(draft.radius_m)}</strong>
            </label>
            <input id={sliderId} type="range" min={minRadius} max={maxRadius} step={step} value={draft.radius_m} onChange={(e) => setDraft((d) => ({ ...d, radius_m: Number(e.target.value) }))} />
          </div>
        </div>
      </Sheet>
    </>
  )
}
