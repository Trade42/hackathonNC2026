// Capacitor wiring for the iOS and Android apps. Loaded only inside the
// native shell, so the website never downloads these plugins.
import { safeNext } from '../lib/next'

const WEB_HOSTS = new Set(['ferry-hacknc.vercel.app'])

// ferry://r/run_demo and https://ferry-hacknc.vercel.app/r/run_demo both
// become /r/run_demo. Anything else is ignored.
export function deepLinkPath(url: string): string | null {
  try {
    const u = new URL(url)
    if (u.protocol === 'ferry:') {
      const path = '/' + [u.host, u.pathname.replace(/^\/+/, '')].filter(Boolean).join('/')
      return safeNext(path + u.search)
    }
    if (u.protocol === 'https:' && WEB_HOSTS.has(u.hostname)) return safeNext(u.pathname + u.search)
  } catch {
    /* not a URL */
  }
  return null
}

let started = false

export async function initNative(navigate: (path: string) => void) {
  if (started) return
  started = true
  const [{ App }, { StatusBar, Style }, { SplashScreen }, { Keyboard }] = await Promise.all([
    import('@capacitor/app'),
    import('@capacitor/status-bar'),
    import('@capacitor/splash-screen'),
    import('@capacitor/keyboard'),
  ])
  // Dark text on our light sky.
  StatusBar.setStyle({ style: Style.Light }).catch(() => {})
  Keyboard.setAccessoryBarVisible({ isVisible: true }).catch(() => {})
  App.addListener('appUrlOpen', ({ url }) => {
    const path = deepLinkPath(url)
    if (path) navigate(path)
  })
  // Android hardware back: go back in the app, leave it from the first screen.
  App.addListener('backButton', ({ canGoBack }) => {
    if (canGoBack) window.history.back()
    else App.exitApp()
  })
  const launch = await App.getLaunchUrl().catch(() => undefined)
  const path = launch?.url ? deepLinkPath(launch.url) : null
  if (path) navigate(path)
  SplashScreen.hide().catch(() => {})
}
