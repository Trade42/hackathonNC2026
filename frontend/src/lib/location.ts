import { useEffect, useRef, useState } from 'react'
import { api } from '../api/endpoints'
import { geolocation } from '../platform'

export type ShareState = 'off' | 'asking' | 'on' | 'denied' | 'unsupported' | 'error'

// While active, post the runner's position every 5 s (only with permission).
export function useShareLocation(runId: string, active: boolean) {
  const [state, setState] = useState<ShareState>(geolocation.supported ? 'off' : 'unsupported')
  const last = useRef<GeolocationPosition | null>(null)

  useEffect(() => {
    if (!active || !geolocation.supported) return
    setState('asking')
    const stop = geolocation.watch(
      (p) => {
        last.current = p
        setState('on')
      },
      (e) => setState(e.code === e.PERMISSION_DENIED ? 'denied' : 'error'),
    )
    const send = () => {
      const p = last.current
      if (!p) return
      api
        .postLocation(runId, {
          location: { lat: p.coords.latitude, lng: p.coords.longitude },
          accuracy_m: Math.round(p.coords.accuracy),
          at: new Date(p.timestamp).toISOString(),
        })
        .catch(() => {})
    }
    const id = window.setInterval(send, 5000)
    return () => {
      stop()
      window.clearInterval(id)
      setState((s) => (s === 'denied' || s === 'unsupported' ? s : 'off'))
    }
  }, [active, runId])

  return state
}
