import { useEffect, useState } from 'react'
import type { LatLng, Restaurant } from '../api/types'
import { currentLocation, metersBetween, walkMinutes } from './geocode'

export type Origin = { at: LatLng; from: 'here' | 'dropoff' }

// Where "Nearest" measures from: the user's live location, asked for only
// while the option is on. Until it arrives, or when it's denied or
// unavailable, their saved drop-off stands in.
export function useNearest(on: boolean, dropoff: LatLng | undefined): Origin | null {
  const [here, setHere] = useState<LatLng | null>(null)
  useEffect(() => {
    if (!on) return
    let live = true
    currentLocation().then(
      (at) => live && setHere(at),
      () => live && setHere(null),
    )
    return () => {
      live = false
    }
  }, [on])
  if (!on) return null
  if (here) return { at: here, from: 'here' }
  return dropoff ? { at: dropoff, from: 'dropoff' } : null
}

// Restaurants nearest-first from the origin. Each copy carries its walk time
// from there in walk_minutes_from_dropoff, the field the cards already show.
export function sortNearest(list: Restaurant[], origin: Origin | null): Restaurant[] {
  if (!origin) return list
  return list
    .map((r) => ({ r, m: metersBetween(origin.at, r.location) }))
    .sort((a, b) => a.m - b.m)
    .map(({ r, m }) => ({ ...r, walk_minutes_from_dropoff: walkMinutes(m) }))
}
