import type { ReactNode } from 'react'
import { Navigate, useLocation } from 'react-router-dom'
import { useMe, useToken } from '../api/hooks'
import { Page } from './TopBar'
import { ErrorState, Loading } from '../ui/States'

// Deep links (/r, /o, /i) land here first: sign in, then onboarding, then back.
export function RequireAuth({ children }: { children: ReactNode }) {
  const token = useToken()
  const loc = useLocation()
  const me = useMe()
  const next = encodeURIComponent(loc.pathname + loc.search)
  if (!token) return <Navigate to={`/signin?next=${next}`} replace />
  if (me.isPending)
    return (
      <Page>
        <div style={{ paddingTop: 72 }}>
          <Loading label="Signing you in" />
        </div>
      </Page>
    )
  if (me.isError)
    return (
      <Page>
        <ErrorState error={me.error} onRetry={() => me.refetch()} />
      </Page>
    )
  if (!me.data.onboarding_complete && loc.pathname !== '/welcome') return <Navigate to={`/welcome?next=${next}`} replace />
  return <>{children}</>
}
