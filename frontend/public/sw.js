// Ferry push service worker (live mode only). Shows notifications the
// backend sends and opens the right deep link when one is tapped.
self.addEventListener('push', (event) => {
  let data = {}
  try {
    data = event.data ? event.data.json() : {}
  } catch {
    data = { title: 'Ferry', body: event.data ? event.data.text() : '' }
  }
  const n = data.notification || data
  event.waitUntil(
    self.registration.showNotification(n.title || 'Ferry', {
      body: n.body || '',
      icon: '/icon-512.png',
      badge: '/favicon.svg',
      data: { url: n.url || (n.data && (n.data.invite_id ? `/i/${n.data.invite_id}` : n.data.order_id ? `/o/${n.data.order_id}` : n.data.run_id ? `/r/${n.data.run_id}` : '/home')) || '/home' },
    }),
  )
})

self.addEventListener('notificationclick', (event) => {
  event.notification.close()
  const url = (event.notification.data && event.notification.data.url) || '/home'
  event.waitUntil(
    self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then((list) => {
      for (const c of list) {
        if ('focus' in c) {
          c.navigate(url)
          return c.focus()
        }
      }
      return self.clients.openWindow(url)
    }),
  )
})
