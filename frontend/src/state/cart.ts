import { useSyncExternalStore } from 'react'

// One cart per restaurant, kept in sessionStorage so rotation, reloads and
// the sign-in detour keep it.
export type CartLine = {
  key: string
  menu_item_id: string
  name: string
  qty: number
  choice_ids: string[]
  choice_names: string[]
  unit_price_cents: number
  note?: string
}

type Carts = Record<string, CartLine[]>
const KEY = 'ferry.carts'
let carts: Carts = read()
const subs = new Set<() => void>()

function read(): Carts {
  try {
    return JSON.parse(sessionStorage.getItem(KEY) ?? '{}')
  } catch {
    return {}
  }
}

function write(next: Carts) {
  carts = next
  try {
    sessionStorage.setItem(KEY, JSON.stringify(next))
  } catch {
    /* memory only */
  }
  subs.forEach((s) => s())
}

const subscribe = (fn: () => void) => {
  subs.add(fn)
  return () => subs.delete(fn)
}

const EMPTY: CartLine[] = []

export const cartStore = {
  lines: (rid: string) => carts[rid] ?? EMPTY,
  add(rid: string, line: Omit<CartLine, 'key'>) {
    const key = `${line.menu_item_id}|${[...line.choice_ids].sort().join(',')}|${line.note ?? ''}`
    const cur = carts[rid] ?? []
    const existing = cur.find((l) => l.key === key)
    const next = existing ? cur.map((l) => (l.key === key ? { ...l, qty: l.qty + line.qty } : l)) : [...cur, { ...line, key }]
    write({ ...carts, [rid]: next })
  },
  setQty(rid: string, key: string, qty: number) {
    const cur = carts[rid] ?? []
    write({ ...carts, [rid]: qty <= 0 ? cur.filter((l) => l.key !== key) : cur.map((l) => (l.key === key ? { ...l, qty } : l)) })
  },
  clear(rid: string) {
    const next = { ...carts }
    delete next[rid]
    write(next)
  },
}

export function useCart(rid: string | undefined) {
  const lines = useSyncExternalStore(subscribe, () => (rid ? cartStore.lines(rid) : EMPTY))
  const subtotal = lines.reduce((a, l) => a + l.unit_price_cents * l.qty, 0)
  const count = lines.reduce((a, l) => a + l.qty, 0)
  return { lines, subtotal, count }
}
