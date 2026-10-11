import 'maplibre-gl/dist/maplibre-gl.css'
import * as maplibregl from 'maplibre-gl'
import workerUrl from 'maplibre-gl/dist/maplibre-gl-worker.mjs?url'
import type { GeoJSONSource, LngLatLike, Map as MLMap, Marker } from 'maplibre-gl'
import { useEffect, useRef, useState } from 'react'
import type { GeoLineString, LatLng } from '../api/types'
import { isReducedMotion } from '../lib/prefs'
import { aeroStyle } from './style'
import s from './FerryMap.module.css'

export type MapMarker = {
  id: string
  at: LatLng
  kind: 'runner' | 'dropoff' | 'restaurant' | 'pin'
  label?: string
  color?: string
  initial?: string
  stale?: boolean
}

type Props = {
  center: LatLng
  zoom?: number
  markers?: MapMarker[]
  route?: GeoLineString | null
  fit?: LatLng[]
  onPick?: (at: LatLng) => void
  label: string
  interactive?: boolean
  // Location picking: a zone of this radius drawn around the map's centre
  // (it follows the map while dragging), and the centre reported when a drag ends.
  zoneRadius_m?: number
  onCenterChange?: (at: LatLng) => void
  // A fixed zone, for showing where a runner drops off.
  zone?: { center: LatLng; radius_m: number } | null
}

// A circle as a GeoJSON polygon (64 points), good enough at walking scale.
function circle(center: LatLng, radius_m: number) {
  const pts: [number, number][] = []
  const dLat = radius_m / 111_320
  const dLng = radius_m / (111_320 * Math.cos((center.lat * Math.PI) / 180))
  for (let i = 0; i <= 64; i++) {
    const a = (i / 64) * Math.PI * 2
    pts.push([center.lng + dLng * Math.cos(a), center.lat + dLat * Math.sin(a)])
  }
  return { type: 'Feature' as const, properties: {}, geometry: { type: 'Polygon' as const, coordinates: [pts] } }
}

// MapLibre finds its worker next to its own module, which bundlers move.
// Point it at the copy Vite emits so dev, web builds and wrappers all work.
maplibregl.setWorkerUrl(workerUrl)

const ll = (p: LatLng): [number, number] => [p.lng, p.lat]

function markerEl(m: MapMarker) {
  const el = document.createElement('div')
  if (m.kind === 'runner') {
    el.className = s.runner
    el.dataset.stale = String(!!m.stale)
    const b = document.createElement('div')
    b.className = s.runnerBubble
    const c = m.color ?? '#1E8FD6'
    b.style.background = `radial-gradient(circle at 32% 28%, #fff 0 8%, rgba(255,255,255,.45) 15%, transparent 32%), radial-gradient(circle at 50% 45%, color-mix(in srgb, ${c} 75%, white), ${c} 75%)`
    b.textContent = m.initial ?? ''
    el.appendChild(b)
  } else {
    el.className = s.pin
    const dot = document.createElement('div')
    dot.className = s.pinDot
    const c = m.kind === 'restaurant' ? '#E0683C' : m.kind === 'pin' ? '#0A5E93' : '#1C6B2A'
    dot.style.background = `radial-gradient(circle at 32% 28%, #fff 0 10%, transparent 34%), ${m.color ?? c}`
    el.appendChild(dot)
    if (m.label) {
      const lab = document.createElement('div')
      lab.className = s.pinLabel
      lab.textContent = m.label
      el.appendChild(lab)
    }
  }
  el.setAttribute('aria-hidden', 'true')
  return el
}

export default function FerryMap({ center, zoom = 15, markers = [], route, fit, onPick, label, interactive = true, zoneRadius_m, onCenterChange, zone }: Props) {
  const box = useRef<HTMLDivElement>(null)
  const mapRef = useRef<MLMap | null>(null)
  const markerRefs = useRef(new Map<string, { marker: Marker; anim?: number; kind: string; stale?: boolean }>())
  const [ready, setReady] = useState(false)
  const [failed, setFailed] = useState(false)
  const pickRef = useRef(onPick)
  pickRef.current = onPick
  const centerRef = useRef(onCenterChange)
  centerRef.current = onCenterChange
  const radiusRef = useRef(zoneRadius_m)
  radiusRef.current = zoneRadius_m

  useEffect(() => {
    let alive = true
    let map: MLMap | null = null
    aeroStyle()
      .then((style) => {
        if (!alive || !box.current) return
        map = new maplibregl.Map({
          container: box.current,
          style,
          center: ll(center),
          zoom,
          attributionControl: false,
          interactive,
          dragRotate: false,
          pitchWithRotate: false,
          cooperativeGestures: false,
          fadeDuration: 0,
        })
        map.touchZoomRotate.disableRotation()
        // Top-right keeps the OSM credit clear of the ETA chip at the bottom.
        map.addControl(new maplibregl.AttributionControl({ compact: true }), 'top-right')
        mapRef.current = map
        map.on('load', () => {
          if (!alive || !map) return
          map.addSource('route', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } })
          map.addLayer({ id: 'route-glow', type: 'line', source: 'route', paint: { 'line-color': '#7CC8F2', 'line-width': 14, 'line-opacity': 0.45, 'line-blur': 6 }, layout: { 'line-cap': 'round', 'line-join': 'round' } })
          map.addLayer({ id: 'route-core', type: 'line', source: 'route', paint: { 'line-color': '#1E8FD6', 'line-width': 4.5 }, layout: { 'line-cap': 'round', 'line-join': 'round' } })
          map.addSource('zone', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } })
          map.addLayer({ id: 'zone-fill', type: 'fill', source: 'zone', paint: { 'fill-color': '#1E8FD6', 'fill-opacity': 0.14 } })
          map.addLayer({ id: 'zone-line', type: 'line', source: 'zone', paint: { 'line-color': '#1E8FD6', 'line-width': 2, 'line-dasharray': [2, 2] } })
          setReady(true)
        })
        // The picking zone follows the map while it moves; the centre is reported when it stops.
        map.on('move', () => {
          const r = radiusRef.current
          if (!map || !r) return
          const c = map.getCenter()
          ;(map.getSource('zone') as GeoJSONSource | undefined)?.setData(circle({ lat: c.lat, lng: c.lng }, r))
        })
        map.on('moveend', () => {
          if (!map) return
          const c = map.getCenter()
          centerRef.current?.({ lat: c.lat, lng: c.lng })
        })
        map.on('click', (e) => pickRef.current?.({ lat: e.lngLat.lat, lng: e.lngLat.lng }))
        map.on('error', () => {
          /* tile hiccups are non-fatal */
        })
      })
      .catch(() => alive && setFailed(true))
    return () => {
      alive = false
      markerRefs.current.forEach((m) => {
        if (m.anim) cancelAnimationFrame(m.anim)
        m.marker.remove()
      })
      markerRefs.current.clear()
      map?.remove()
      mapRef.current = null
    }
    // The map is created once; props below update it in place.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  // Route line
  useEffect(() => {
    const map = mapRef.current
    if (!ready || !map) return
    const src = map.getSource('route') as GeoJSONSource | undefined
    src?.setData(route ? { type: 'Feature', properties: {}, geometry: route } : { type: 'FeatureCollection', features: [] })
  }, [route, ready])

  // Zone: a fixed one, or the picking zone around the centre.
  useEffect(() => {
    const map = mapRef.current
    if (!ready || !map) return
    const src = map.getSource('zone') as GeoJSONSource | undefined
    if (zone) src?.setData(circle(zone.center, zone.radius_m))
    else if (zoneRadius_m) {
      const c = map.getCenter()
      src?.setData(circle({ lat: c.lat, lng: c.lng }, zoneRadius_m))
    } else src?.setData({ type: 'FeatureCollection', features: [] })
  }, [ready, zone, zoneRadius_m])

  // Markers: create, move (tweened for the runner), remove.
  useEffect(() => {
    const map = mapRef.current
    if (!ready || !map) return
    const seen = new Set<string>()
    for (const m of markers) {
      seen.add(m.id)
      const existing = markerRefs.current.get(m.id)
      if (!existing || existing.kind !== m.kind || existing.stale !== m.stale) {
        existing?.marker.remove()
        const marker = new maplibregl.Marker({ element: markerEl(m), anchor: m.kind === 'runner' ? 'center' : 'top' }).setLngLat(ll(m.at)).addTo(map)
        markerRefs.current.set(m.id, { marker, kind: m.kind, stale: m.stale })
        continue
      }
      const from = existing.marker.getLngLat()
      const to = ll(m.at)
      if (from.lng === to[0] && from.lat === to[1]) continue
      if (existing.anim) cancelAnimationFrame(existing.anim)
      if (m.kind !== 'runner' || isReducedMotion()) {
        existing.marker.setLngLat(to)
        continue
      }
      const start = performance.now()
      const dur = 900
      const step = (t: number) => {
        const p = Math.min(1, (t - start) / dur)
        existing.marker.setLngLat([from.lng + (to[0] - from.lng) * p, from.lat + (to[1] - from.lat) * p] as LngLatLike)
        if (p < 1) existing.anim = requestAnimationFrame(step)
      }
      existing.anim = requestAnimationFrame(step)
    }
    for (const [id, m] of markerRefs.current) {
      if (!seen.has(id)) {
        m.marker.remove()
        markerRefs.current.delete(id)
      }
    }
  }, [markers, ready])

  // Fit the view to the points that matter (runner, stops, restaurant).
  const fitKey = fit?.map((p) => `${p.lat.toFixed(4)},${p.lng.toFixed(4)}`).join('|')
  useEffect(() => {
    const map = mapRef.current
    if (!ready || !map || !fit?.length) return
    if (fit.length === 1) {
      map.easeTo({ center: ll(fit[0]), duration: isReducedMotion() ? 0 : 400 })
      return
    }
    const b = new maplibregl.LngLatBounds(ll(fit[0]), ll(fit[0]))
    fit.forEach((p) => b.extend(ll(p)))
    map.fitBounds(b, { padding: { top: 64, bottom: 96, left: 64, right: 120 }, maxZoom: 16.5, duration: isReducedMotion() ? 0 : 400 })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fitKey, ready])

  // Keep the canvas sized to its box (rotation, sheet resizes).
  useEffect(() => {
    if (!box.current) return
    const ro = new ResizeObserver(() => mapRef.current?.resize())
    ro.observe(box.current)
    return () => ro.disconnect()
  }, [])

  return (
    <div className={s.wrap} role="region" aria-label={label}>
      <div ref={box} className={s.map} />
      {failed && <div className={s.fallback}>The map couldn’t load. Live distance and ETA still update below.</div>}
    </div>
  )
}
