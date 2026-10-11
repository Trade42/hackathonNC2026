import type { StyleSpecification } from 'maplibre-gl'

// OpenFreeMap (no key). Verified: https://tiles.openfreemap.org/styles/positron
export const STYLE_URL = 'https://tiles.openfreemap.org/styles/positron'

let cached: Promise<StyleSpecification> | null = null

// Restyle Positron in Aero tones: sky-tinted land, clear water, fresh parks.
export function aeroStyle(): Promise<StyleSpecification> {
  cached ??= fetch(STYLE_URL)
    .then((r) => r.json() as Promise<StyleSpecification>)
    .then((style) => {
      // Shield layers carry filters MapLibre warns about, and campus maps don't need them.
      style.layers = style.layers.filter((l) => !/shield/.test(l.id))
      for (const layer of style.layers) {
        const id = layer.id
        const paint = ((layer as { paint?: Record<string, unknown> }).paint ??= {})
        if (id === 'background') paint['background-color'] = '#EEF8FD'
        else if (id === 'water') paint['fill-color'] = '#8FD8EE'
        else if (id === 'waterway') paint['line-color'] = '#8FD8EE'
        else if (id === 'park' || id === 'landcover_wood') {
          paint['fill-color'] = '#CDEFC0'
          paint['fill-opacity'] = 0.9
        } else if (id === 'landuse_residential') paint['fill-color'] = '#E6F3FA'
        else if (id === 'building') {
          paint['fill-color'] = '#DCEBF4'
          paint['fill-outline-color'] = '#C8DCE8'
        } else if (id.startsWith('highway') && layer.type === 'line' && !id.includes('casing')) paint['line-color'] = '#FFFFFF'
        else if (id.includes('casing') && layer.type === 'line') paint['line-color'] = '#CFE3EF'
        else if (layer.type === 'symbol') {
          paint['text-color'] = '#3A5B72'
          paint['text-halo-color'] = '#FFFFFF'
        }
      }
      return style
    })
  return cached
}
