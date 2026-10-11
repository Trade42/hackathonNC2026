import { lazy, Suspense, useEffect } from 'react'
import { Navigate, Route, Routes, useLocation, useNavigate } from 'react-router-dom'
import { useToken } from './api/hooks'
import { realtime } from './api/realtime'
import { useReducedMotion } from './lib/prefs'
import { platform } from './platform'
import { RequireAuth } from './shell/RequireAuth'
import { Page } from './shell/TopBar'
import { Landing } from './screens/Landing'
import { OfflineBanner } from './ui/States'
import { RunnerMark } from './ui/RunnerMark'

const AppShell = lazy(() => import('./shell/AppShell'))
const PopLayer = lazy(() => import('./ui/PopLayer'))
const SignIn = lazy(() => import('./screens/SignIn'))
const InstallIOS = lazy(() => import('./screens/InstallIOS'))
const Onboarding = lazy(() => import('./screens/Onboarding'))
const Home = lazy(() => import('./screens/Home'))
const AreaScreen = lazy(() => import('./screens/Area'))
const RestaurantScreen = lazy(() => import('./screens/Restaurant'))
const JoinRun = lazy(() => import('./screens/JoinRun'))
const Checkout = lazy(() => import('./screens/Checkout'))
const OrderTrack = lazy(() => import('./screens/OrderTrack'))
const Orders = lazy(() => import('./screens/Orders'))
const WalletScreen = lazy(() => import('./screens/Wallet'))
const ImpactScreen = lazy(() => import('./screens/Impact'))
const Circles = lazy(() => import('./screens/Circles'))
const CircleDetail = lazy(() => import('./screens/CircleDetail'))
const InviteScreen = lazy(() => import('./screens/Invite'))
const Runner = lazy(() => import('./screens/Runner'))
const NewRun = lazy(() => import('./screens/NewRun'))
const Settings = lazy(() => import('./screens/Settings'))
const Search = lazy(() => import('./screens/Search'))
const You = lazy(() => import('./screens/You'))
const NotFound = lazy(() => import('./screens/NotFound'))

// Between screens: the runner mark runs while the next screen loads.
function Fallback() {
  return (
    <Page>
      <div role="status" aria-live="polite" style={{ display: 'grid', placeItems: 'center', gap: 12, paddingTop: 120 }}>
        <RunnerMark size={56} animate="loop" />
        <span className="muted">Loading</span>
      </div>
    </Page>
  )
}

// Inside the iOS and Android apps: status bar, deep links, splash, back button.
function NativeBridge() {
  const nav = useNavigate()
  useEffect(() => {
    if (!platform.isNative) return
    void import('./platform/native').then((m) => m.initNative((path) => nav(path)))
  }, [nav])
  return null
}

function ScrollToTop() {
  const { pathname } = useLocation()
  useEffect(() => {
    window.scrollTo(0, 0)
  }, [pathname])
  return null
}

export default function App() {
  const token = useToken()
  const reduced = useReducedMotion()

  useEffect(() => {
    realtime.connect(token)
  }, [token])
  useEffect(() => {
    document.documentElement.dataset.motion = reduced ? 'reduced' : 'full'
  }, [reduced])

  const authed = (el: React.ReactNode) => <RequireAuth>{el}</RequireAuth>

  return (
    <>
      <ScrollToTop />
      <NativeBridge />
      <OfflineBanner />
      <Suspense fallback={<Fallback />}>
        <Routes>
          {/* The website is the landing page; the apps open straight into Ferry. */}
          <Route path="/" element={platform.isNative ? <Navigate to={token ? '/home' : '/signin'} replace /> : <Landing />} />
          <Route path="/signin" element={<SignIn />} />
          <Route path="/install/ios" element={<InstallIOS />} />
          <Route path="/welcome" element={authed(<Onboarding />)} />
          <Route element={authed(<AppShell />)}>
            <Route path="/home" element={<Home />} />
            <Route path="/food/:areaId" element={<AreaScreen />} />
            <Route path="/food/:areaId/:restaurantId" element={<RestaurantScreen />} />
            <Route path="/food/:areaId/:restaurantId/request" element={<Checkout />} />
            <Route path="/r/:runId" element={<JoinRun />} />
            <Route path="/r/:runId/checkout" element={<Checkout />} />
            <Route path="/o/:orderId" element={<OrderTrack />} />
            <Route path="/orders" element={<Orders />} />
            <Route path="/wallet" element={<WalletScreen />} />
            <Route path="/impact" element={<ImpactScreen />} />
            <Route path="/circles" element={<Circles />} />
            <Route path="/circles/:circleId" element={<CircleDetail />} />
            <Route path="/i/:inviteId" element={<InviteScreen />} />
            <Route path="/run/new" element={<NewRun />} />
            <Route path="/run/:runId" element={<Runner />} />
            <Route path="/settings" element={<Settings />} />
            <Route path="/search" element={<Search />} />
            <Route path="/you" element={<You />} />
          </Route>
          <Route path="/app" element={<Navigate to="/home" replace />} />
          <Route path="*" element={<NotFound />} />
        </Routes>
      </Suspense>
      <Suspense fallback={null}>
        <PopLayer />
      </Suspense>
    </>
  )
}
