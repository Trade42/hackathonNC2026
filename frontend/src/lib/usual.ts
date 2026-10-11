import type { QueryClient } from '@tanstack/react-query'
import { api } from '../api/endpoints'
import { keys } from '../api/hooks'
import type { Order } from '../api/types'
import { cartStore } from '../state/cart'

// Refills a restaurant's cart with a past order's lines, using today's menu
// for names and prices. Items no longer on the menu are skipped.
export async function refillCart(qc: QueryClient, restaurantId: string, order: Pick<Order, 'lines'>) {
  const menu = await qc.fetchQuery({ queryKey: keys.menu(restaurantId), queryFn: () => api.menu(restaurantId), staleTime: 5 * 60e3 })
  const items = new Map(menu.categories.flatMap((c) => c.items).map((i) => [i.id, i]))
  cartStore.clear(restaurantId)
  let added = 0
  for (const l of order.lines) {
    const item = items.get(l.menu_item_id)
    if (!item) continue
    const choices = item.options.flatMap((o) => o.choices).filter((c) => l.choice_ids.includes(c.id))
    cartStore.add(restaurantId, {
      menu_item_id: item.id,
      name: item.name,
      qty: l.qty,
      choice_ids: choices.map((c) => c.id),
      choice_names: choices.map((c) => c.name),
      unit_price_cents: item.price_cents + choices.reduce((a, c) => a + c.price_delta_cents, 0),
      note: l.note,
    })
    added++
  }
  return added
}
