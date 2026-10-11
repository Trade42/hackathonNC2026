import type { Notification } from '../api/types'

// Where tapping a notification goes.
export function notificationHref(n: Notification): string | null {
  const d = n.data as Record<string, string | undefined>
  if (d.invite_id) return `/i/${d.invite_id}`
  if (d.order_id) return `/o/${d.order_id}`
  if (n.type === 'system' && d.run_id && d.new_run_restaurant_id) return '/run/new?restaurant=' + d.new_run_restaurant_id
  if (d.run_id) return `/r/${d.run_id}`
  if (n.type === 'payout') return '/wallet'
  return null
}
