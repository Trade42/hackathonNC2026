import type { Order } from '../api/types'

export const ACTIVE: Order['status'][] = ['funded', 'purchased', 'picked_up', 'delivered']

export const statusText: Record<Order['status'], string> = {
  funded: 'Order placed',
  purchased: 'Bought',
  picked_up: 'On the way',
  delivered: 'Delivered',
  settled: 'Delivered',
  cancelled: 'Cancelled',
  refunded: 'Refunded',
}

// The delivery steps a customer sees, in order.
export const STEPS: Order['status'][] = ['funded', 'purchased', 'picked_up', 'delivered']
export const stepIndex = (status: Order['status']) => Math.max(0, STEPS.indexOf(status === 'settled' ? 'delivered' : status))

// One line that always says a person is bringing the food (Ferry only organizes it).
export function orderHeadline(o: Pick<Order, 'status' | 'runner' | 'dropoff'>, restaurant?: string) {
  const who = o.runner.display_name
  switch (o.status) {
    case 'funded':
      return `${who} will pick up your order${restaurant ? ` at ${restaurant}` : ''}`
    case 'purchased':
      return `${who} bought your food`
    case 'picked_up':
      return `${who} is bringing your food`
    case 'delivered':
    case 'settled':
      return `${who} is at ${o.dropoff.label}`
    default:
      return statusText[o.status]
  }
}
