import { lazy, Suspense, type ComponentProps } from 'react'
import { Skeleton } from '../ui/States'

const FerryMap = lazy(() => import('./FerryMap'))

// MapLibre is large; load it only on screens that show a map.
export function LazyMap(props: ComponentProps<typeof FerryMap>) {
  return (
    <Suspense fallback={<Skeleton h="100%" r="inherit" style={{ minHeight: 220 }} />}>
      <FerryMap {...props} />
    </Suspense>
  )
}

export type { MapMarker } from './FerryMap'
