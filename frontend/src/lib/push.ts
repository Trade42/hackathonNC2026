import { api } from '../api/endpoints'
import { env } from '../config/env'
import { notifications, platform } from '../platform'

const b64ToBytes = (b64: string) => {
  const pad = '='.repeat((4 - (b64.length % 4)) % 4)
  const raw = atob((b64 + pad).replace(/-/g, '+').replace(/_/g, '/'))
  return Uint8Array.from(raw, (c) => c.charCodeAt(0))
}

// Web Push where supported. In mock mode MSW owns the service worker, so we
// only ask for permission and register without a push subscription.
export async function enablePush(): Promise<'on' | 'denied' | 'unsupported'> {
  const perm = await notifications.request()
  if (perm === 'unsupported') return 'unsupported'
  if (perm !== 'granted') return 'denied'
  const plat = platform.isIOS ? 'ios' : platform.isAndroid ? 'android' : 'web'
  if (env.isMock || !('PushManager' in window)) {
    await api.pushSubscribe({ platform: plat }).catch(() => {})
    return 'on'
  }
  const reg = await navigator.serviceWorker.register('/sw.js')
  const { vapid_public_key } = await api.pushKey()
  const sub = (await reg.pushManager.getSubscription()) ?? (await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64ToBytes(vapid_public_key) }))
  await api.pushSubscribe({ platform: 'web', web_push: sub.toJSON() })
  return 'on'
}
