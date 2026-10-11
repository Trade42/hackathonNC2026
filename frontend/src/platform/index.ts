// The one place for platform differences: device detection, safe areas,
// messaging compose links, deep links and notifications. The web and the
// Capacitor apps (iOS, Android) all run this same build.

type W = Window & { Capacitor?: { isNativePlatform?: () => boolean; getPlatform?: () => string } }
const w = (typeof window !== 'undefined' ? window : undefined) as W | undefined
const ua = typeof navigator !== 'undefined' ? navigator.userAgent : ''

const capPlatform = w?.Capacitor?.isNativePlatform?.() ? w.Capacitor.getPlatform?.() : undefined

export const platform = {
  isIOS: capPlatform === 'ios' || /iPhone|iPad|iPod/.test(ua) || (/Macintosh/.test(ua) && typeof navigator !== 'undefined' && navigator.maxTouchPoints > 1),
  isAndroid: capPlatform === 'android' || /Android/.test(ua),
  isNative: !!capPlatform,
  shell: (capPlatform ?? 'web') as 'ios' | 'android' | 'web',
  get isPhone() {
    return this.isIOS || this.isAndroid
  },
}

export type Compose = { phones: string[]; body: string }

// Contract compose formats (the meet-again fallback when there is no Telegram
// link). Returns null on desktop, where we show a QR code.
export function composeLink({ phones, body }: Compose): string | null {
  const b = encodeURIComponent(body)
  if (platform.isIOS) {
    if (phones.length === 1) return `sms:${phones[0]}&body=${b}`
    return `sms://open?addresses=${phones.join(',')}&body=${b}`
  }
  if (platform.isAndroid) return `sms:${phones.join(';')}?body=${b}`
  return null
}

// What the QR code on desktop encodes: a plain sms: URI both phone OSes open.
export function composeQrPayload({ phones, body }: Compose) {
  return `sms:${phones.join(',')}?body=${encodeURIComponent(body)}`
}

export function openExternal(url: string) {
  // In the app, hand links to iOS or Android so Telegram opens in its own app.
  if (platform.isNative) {
    void import('@capacitor/app-launcher')
      .then(({ AppLauncher }) => AppLauncher.openUrl({ url }))
      .catch(() => {
        window.location.href = url
      })
    return
  }
  window.location.href = url
}

export async function share(data: { title: string; text?: string; url: string }) {
  if (platform.isNative) {
    try {
      const { Share } = await import('@capacitor/share')
      await Share.share({ title: data.title, text: data.text, url: data.url })
      return true
    } catch {
      return false
    }
  }
  if (navigator.share) {
    try {
      await navigator.share(data)
      return true
    } catch {
      return false
    }
  }
  try {
    await navigator.clipboard.writeText(data.url)
    return true
  } catch {
    return false
  }
}

export function vibrate(pattern: number | number[]) {
  if (platform.isNative) {
    // A light tap for short patterns, a success buzz for longer ones.
    void import('@capacitor/haptics')
      .then(({ Haptics, ImpactStyle, NotificationType }) =>
        Array.isArray(pattern) || pattern > 30 ? Haptics.notification({ type: NotificationType.Success }) : Haptics.impact({ style: ImpactStyle.Light }),
      )
      .catch(() => {})
    return
  }
  try {
    navigator.vibrate?.(pattern)
  } catch {
    /* unsupported */
  }
}

export const notifications = {
  supported: typeof window !== 'undefined' && 'Notification' in window && 'serviceWorker' in navigator,
  get permission(): NotificationPermission | 'unsupported' {
    return this.supported ? Notification.permission : 'unsupported'
  },
  async request(): Promise<NotificationPermission | 'unsupported'> {
    if (!this.supported) return 'unsupported'
    try {
      return await Notification.requestPermission()
    } catch {
      return 'denied'
    }
  },
}

export const geolocation = {
  supported: typeof navigator !== 'undefined' && 'geolocation' in navigator,
  watch(onPos: (p: GeolocationPosition) => void, onErr: (e: GeolocationPositionError) => void) {
    if (!this.supported) return () => {}
    const id = navigator.geolocation.watchPosition(onPos, onErr, { enableHighAccuracy: true, maximumAge: 4000, timeout: 15000 })
    return () => navigator.geolocation.clearWatch(id)
  },
}

// Download target for the landing page.
export function detectDownload(): 'ios' | 'android' | 'other' {
  if (platform.isIOS) return 'ios'
  if (platform.isAndroid) return 'android'
  return 'other'
}
