import type { Place } from '../api/types'

// Drop-off places from the Contract v1.1 fixtures. GET /v1/places is the real
// list; this one is only the fallback for mock mode and for when it's offline.
export const DROPOFFS: Place[] = [
  { id: 'place_fetzer', label: 'Fetzer Gym (HackNC)', location: { lat: 35.9093, lng: -79.0468 } },
  { id: 'place_hinton', label: 'Hinton James lobby', location: { lat: 35.9023, lng: -79.0436 } },
  { id: 'place_davis', label: 'Davis Library steps', location: { lat: 35.9108, lng: -79.0481 } },
  { id: 'place_ehaus', label: 'Ehringhaus lobby', location: { lat: 35.9046, lng: -79.0447 } },
]

// The server's places when it has sent them, else the fallback list.
export const knownPlaces = (server: Place[] | undefined): Place[] => (server?.length ? server : DROPOFFS)

export const CAMPUS_CENTER = { lat: 35.9095, lng: -79.0479 }
