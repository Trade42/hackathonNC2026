// Fields the frontend has asked for in docs/CONTRACT_REQUESTS.md ("Going
// somewhere: return time, return place and drop-off radius", "Profile: how
// far I'll walk to drop off"). All optional and additive. Until the backend
// adds them, mock mode stores them and live mode simply ignores them. Delete
// this file once `npm run sync:contract` brings them in.
import type { components } from './schema'

type S = components['schemas']

// On a run: when the runner expects to be back, where, and how far from
// there they'll walk to hand food over.
export type RunPlan = {
  returning_at?: string | null
  return_place?: S['Place'] | null
  dropoff_radius_m?: number | null
}

// On a profile: the default for dropoff_radius_m when you go somewhere.
export type ProfileExtra = {
  max_dropoff_distance_m?: number | null
}

// On an order or request: how far from the drop-off point you're happy to meet.
export type DropoffZone = {
  dropoff_radius_m?: number | null
}
