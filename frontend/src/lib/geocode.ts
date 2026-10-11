import type { LatLng, Place } from '../api/types'
import { DROPOFFS } from './places'

// Address search for the drop-off and return-place pickers, through Photon
// (komoot's open OpenStreetMap geocoder; no key, CORS allowed), limited to
// the Chapel Hill and Carrboro area. Only the typed text and the map centre
// are sent. If Photon is slow or down, the known campus spots still match.
// CONTRACT_REQUESTS asks the backend for a proxy so the client calls only Ferry.

const BASE = 'https://photon.komoot.io'
const BBOX = '-79.15,35.85,-78.95,35.98'
const TIMEOUT = 4000

type Feature = {
  geometry: { coordinates: [number, number] }
  properties: { name?: string; street?: string; housenumber?: string; city?: string }
}

const labelOf = (p: Feature['properties']) => {
  const street = [p.housenumber, p.street].filter(Boolean).join(' ')
  return [p.name, street && street !== p.name ? street : null].filter(Boolean).join(', ') || p.city || 'Dropped pin'
}

const toPlace = (f: Feature): Place => ({
  label: labelOf(f.properties),
  location: { lat: f.geometry.coordinates[1], lng: f.geometry.coordinates[0] },
})

async function get(url: string): Promise<Feature[]> {
  const ctl = new AbortController()
  const t = window.setTimeout(() => ctl.abort(), TIMEOUT)
  try {
    const res = await fetch(url, { signal: ctl.signal })
    if (!res.ok) throw new Error(String(res.status))
    return ((await res.json()) as { features: Feature[] }).features
  } finally {
    window.clearTimeout(t)
  }
}

function localMatches(q: string, extra: Place[]): Place[] {
  const n = q.trim().toLowerCase()
  return (extra.length ? extra : DROPOFFS).filter((p) => p.label.toLowerCase().includes(n)).slice(0, 6)
}

export async function searchPlaces(q: string, near: LatLng, extra: Place[] = []): Promise<Place[]> {
  if (q.trim().length < 2) return []
  const local = localMatches(q, extra)
  try {
    const feats = await get(`${BASE}/api/?q=${encodeURIComponent(q)}&lat=${near.lat}&lon=${near.lng}&limit=6&bbox=${BBOX}&lang=en`)
    const found = feats.map(toPlace)
    // Known spots first, then the map's results without repeats.
    return [...local, ...found.filter((f) => !local.some((l) => l.label === f.label))].slice(0, 8)
  } catch {
    return local
  }
}

export async function placeAt(at: LatLng): Promise<Place> {
  try {
    const feats = await get(`${BASE}/reverse?lat=${at.lat}&lon=${at.lng}&limit=1&lang=en`)
    if (feats[0]) return { label: labelOf(feats[0].properties), location: at }
  } catch {
    /* fall through */
  }
  return { label: 'Dropped pin', location: at }
}

// Straight-line distance in metres (fine at walking scale).
export function metersBetween(a: LatLng, b: LatLng) {
  const R = 6_371_000
  const dLat = ((b.lat - a.lat) * Math.PI) / 180
  const dLng = ((b.lng - a.lng) * Math.PI) / 180
  const s = Math.sin(dLat / 2) ** 2 + Math.cos((a.lat * Math.PI) / 180) * Math.cos((b.lat * Math.PI) / 180) * Math.sin(dLng / 2) ** 2
  return 2 * R * Math.asin(Math.sqrt(s))
}

// About 80 m a minute at a normal walk.
export const walkMinutes = (m: number) => Math.max(1, Math.round(m / 80))

export function currentLocation(): Promise<LatLng> {
  return new Promise((resolve, reject) => {
    if (!('geolocation' in navigator)) return reject(new Error('unsupported'))
    navigator.geolocation.getCurrentPosition(
      (p) => resolve({ lat: p.coords.latitude, lng: p.coords.longitude }),
      reject,
      { enableHighAccuracy: true, timeout: 10_000, maximumAge: 60_000 },
    )
  })
}

// True when the browser already allows location, so we can use it without a prompt.
export async function locationAllowed() {
  try {
    const st = await navigator.permissions?.query({ name: 'geolocation' as PermissionName })
    return st?.state === 'granted'
  } catch {
    return false
  }
}
