// Stateful mock backend. State lives in localStorage so reloads keep the demo
// where it was and two tabs (judge and Maya) share one world. Events reach the
// other tab's mock WebSocket through a BroadcastChannel.
import type {
  Circle,
  Impact,
  ImpactScope,
  Invite,
  LatLng,
  Notification,
  Order,
  OrderStatus,
  Place,
  Run,
  RunDetail,
  StreamEvent,
  Track,
  User,
  Wallet,
  WalletTxn,
} from '../api/types'
import {
  IMPACT_METHOD,
  demoRoute,
  demoRunFixture,
  menus,
  places,
  restaurants,
  seedCircles,
  seedUsers,
  users,
} from './fixtures'
import { ferryFeeCents, runnerPayCents } from '../lib/pricing'

const KEY = 'ferry.mock.v6'
const TAX = 0.075
export const CO2_PER_DELIVERY_G = 480
const WALK_SPEED_MPS = 1.6 // Maya walks briskly
export const WALK_SIM_MS = 60_000 // the 1 km walk plays out in a minute

type Sim = {
  auto: boolean
  phase: 'idle' | 'ordered' | 'closed' | 'purchased' | 'walking' | 'arrived' | 'done'
  nextAt: number
  walkStart: number
  realLocation: boolean
  inviteAt: number | null
  threadReadyAt: number | null
  tickOwner: string
  tickUntil: number
}

export type MockState = {
  seq: number
  users: Record<string, User>
  runs: Record<string, Run>
  orders: Record<string, Order>
  codes: Record<string, string>
  receiptTotals: Record<string, number>
  wallets: Record<string, Wallet>
  notifications: Record<string, Notification[]>
  invites: Record<string, Invite>
  circles: Circle[]
  challenges: Record<string, { created: number; userId: string; isNew: boolean; opened?: number }>
  receipts: Record<string, string>
  sim: Sim
}

export type Audience = string[]
type Outgoing = { event: StreamEvent; to: Audience }

const tabId = Math.random().toString(36).slice(2)
const channel = typeof BroadcastChannel !== 'undefined' ? new BroadcastChannel('ferry.mock') : null
const listeners = new Set<(o: Outgoing) => void>()

channel?.addEventListener('message', (e: MessageEvent<Outgoing>) => listeners.forEach((l) => l(e.data)))

export function onEvent(fn: (o: Outgoing) => void) {
  listeners.add(fn)
  return () => listeners.delete(fn)
}

let memory: MockState | null = null

export function load(): MockState {
  try {
    const raw = localStorage.getItem(KEY)
    if (raw) return (memory = JSON.parse(raw) as MockState)
  } catch {
    /* fall through to memory or seed */
  }
  if (memory) return memory
  memory = seed(Date.now())
  save(memory)
  return memory
}

function save(s: MockState) {
  memory = s
  try {
    localStorage.setItem(KEY, JSON.stringify(s))
  } catch {
    // Quota: drop stored receipt photos and retry once.
    s.receipts = {}
    try {
      localStorage.setItem(KEY, JSON.stringify(s))
    } catch {
      /* keep in memory */
    }
  }
}

export function reset() {
  const s = seed(Date.now())
  save(s)
  return s
}

// Run a mutation, persist, then deliver the events it produced.
export function mutate<T>(fn: (s: MockState, emit: (event: StreamEvent, to: Audience) => void) => T): T {
  const s = load()
  const out: Outgoing[] = []
  const result = fn(s, (event, to) => out.push({ event, to }))
  save(s)
  for (const o of out) {
    listeners.forEach((l) => l(o))
    channel?.postMessage(o)
  }
  return result
}

const iso = (t: number) => new Date(t).toISOString()
const id = (s: MockState, prefix: string) => `${prefix}${(++s.seq).toString(36)}${Math.random().toString(36).slice(2, 6)}`

// ---------- seed ----------

function seed(now: number): MockState {
  const s: MockState = {
    seq: 100,
    users: seedUsers(),
    runs: {},
    orders: {},
    codes: {},
    receiptTotals: {},
    wallets: {},
    notifications: {},
    invites: {},
    circles: seedCircles(now),
    challenges: {},
    receipts: {},
    sim: {
      auto: false,
      phase: 'idle',
      nextAt: 0,
      walkStart: 0,
      realLocation: false,
      inviteAt: null,
      threadReadyAt: null,
      tickOwner: '',
      tickUntil: 0,
    },
  }
  const cosmic = restaurants.find((r) => r.id === 'rst_cosmic')!
  const insomnia = restaurants.find((r) => r.id === 'rst_insomnia')!
  const lime = restaurants.find((r) => r.id === 'rst_limebasil')!
  const supdogs = restaurants.find((r) => r.id === 'rst_supdogs')!

  // The contract's demo run, with its times shifted to now (as the backend seeder does).
  // Going-somewhere plans (requested contract fields): back when, where, and how far they'll walk.
  s.runs.run_demo = {
    ...structuredClone(demoRunFixture),
    restaurant: cosmic,
    leaving_at: iso(now + 4 * 60e3),
    window_closes_at: iso(now + 10 * 60e3),
    returning_at: iso(now + 32 * 60e3),
    return_place: places.place_fetzer,
    dropoff_radius_m: 500,
  }
  for (const [uid, m] of [['usr_judge', 400], ['usr_maya', 500], ['usr_dana', 300], ['usr_leo', 300]] as const) {
    if (s.users[uid]) s.users[uid].profile.max_dropoff_distance_m = m
  }
  s.runs.run_insomnia1 = {
    id: 'run_insomnia1',
    restaurant: insomnia,
    runner: users.usr_leo,
    circle_id: 'cir_hinton4',
    status: 'open',
    leaving_at: iso(now + 22 * 60e3),
    window_closes_at: iso(now + 25 * 60e3),
    returning_at: iso(now + 45 * 60e3),
    return_place: places.place_hinton,
    dropoff_radius_m: 300,
    order_count: 0,
    created_via: 'app',
    runner_location: null,
    eta_minutes: null,
  }
  s.runs.run_climb1 = {
    id: 'run_climb1',
    restaurant: lime,
    runner: users.usr_maya,
    circle_id: 'cir_climb',
    status: 'open',
    leaving_at: iso(now + 40 * 60e3),
    window_closes_at: iso(now + 45 * 60e3),
    returning_at: iso(now + 75 * 60e3),
    return_place: places.place_davis,
    dropoff_radius_m: 400,
    order_count: 0,
    created_via: 'message',
    runner_location: null,
    eta_minutes: null,
  }
  s.runs.run_past1 = {
    id: 'run_past1',
    restaurant: supdogs,
    runner: users.usr_dana,
    circle_id: 'cir_hinton4',
    status: 'delivered',
    leaving_at: iso(now - 6 * 864e5),
    window_closes_at: iso(now - 6 * 864e5 - 10 * 60e3),
    order_count: 1,
    created_via: 'message',
    runner_location: null,
    eta_minutes: null,
  }

  // Dana already joined Maya's run, so the runner has a list to shop.
  placeOrderInternal(s, 'run_demo', 'usr_dana', [{ menu_item_id: 'itm_cosmic_bean_cheese_burrito', qty: 1, choice_ids: ['cho_cosmic_bean_cheese_burrito_black_beans'] }], 200, places.place_fetzer, now - 3 * 60e3, () => {})

  // A finished order from last week for Jordan's history.
  const past = placeOrderInternal(s, 'run_past1', 'usr_judge', [
    { menu_item_id: 'itm_supdogs_classic_dog', qty: 2, choice_ids: ['cho_supdogs_classic_dog_chili'] },
    { menu_item_id: 'itm_supdogs_tots', qty: 1, choice_ids: [] },
  ], 150, places.place_fetzer, now - 6 * 864e5, () => {}, true)
  const t0 = now - 6 * 864e5
  past.status = 'settled'
  past.co2_saved_g = CO2_PER_DELIVERY_G
  past.eta_minutes = null
  past.timeline = [
    { status: 'funded', at: iso(t0) },
    { status: 'purchased', at: iso(t0 + 14 * 60e3) },
    { status: 'picked_up', at: iso(t0 + 19 * 60e3) },
    { status: 'delivered', at: iso(t0 + 31 * 60e3) },
    { status: 'settled', at: iso(t0 + 31 * 60e3) },
  ]

  const w = (label: string, txns: WalletTxn[]): Wallet => ({
    balance_cents: txns.reduce((a, t) => a + t.amount_cents, 0),
    account_label: label,
    mode: 'nessie_mock',
    transactions: txns.sort((a, b) => b.at.localeCompare(a.at)),
  })
  const judgeHold = past.total_cents
  s.wallets.usr_judge = w('Nessie checking ending 4821', [
    { id: 'txn_j1', kind: 'deposit', amount_cents: 5000, description: 'Sandbox deposit from Capital One Nessie', at: iso(now - 8 * 864e5) },
    { id: 'txn_j2', kind: 'order_hold', amount_cents: -judgeHold, description: 'Hold for your Sup Dogs order', at: iso(t0), run_id: 'run_past1', order_id: past.id },
    { id: 'txn_j3', kind: 'refund', amount_cents: 42, description: 'Receipt came in under the estimate', at: iso(t0 + 31 * 60e3), run_id: 'run_past1', order_id: past.id },
  ])
  s.wallets.usr_maya = w('Nessie checking ending 7730', [
    { id: 'txn_m1', kind: 'deposit', amount_cents: 2500, description: 'Sandbox deposit from Capital One Nessie', at: iso(now - 9 * 864e5) },
    { id: 'txn_m2', kind: 'payout', amount_cents: 350, description: 'Fee and tip from Leo', at: iso(now - 3 * 864e5) },
  ])
  s.wallets.usr_dana = w('Nessie checking ending 1186', [
    { id: 'txn_d1', kind: 'deposit', amount_cents: 4000, description: 'Sandbox deposit from Capital One Nessie', at: iso(now - 8 * 864e5) },
    { id: 'txn_d2', kind: 'order_hold', amount_cents: -s.orders[Object.keys(s.orders)[0]].total_cents, description: 'Hold for your Cosmic Cantina order', at: iso(now - 3 * 60e3), run_id: 'run_demo' },
  ])
  s.wallets.usr_leo = w('Nessie checking ending 5502', [
    { id: 'txn_l1', kind: 'deposit', amount_cents: 3000, description: 'Sandbox deposit from Capital One Nessie', at: iso(now - 7 * 864e5) },
  ])

  s.notifications.usr_judge = [
    {
      id: 'ntf_seed1',
      type: 'run_open',
      title: 'Maya is heading to Cosmic Cantina',
      body: 'Add your order before the window closes.',
      data: { run_id: 'run_demo' },
      created_at: iso(now - 2 * 60e3),
      read: false,
    },
  ]
  return s
}

// ---------- projections (what a given viewer may see) ----------

export const userFromToken = (s: MockState, auth: string | null): User | null => {
  const t = auth?.replace(/^Bearer\s+/i, '') ?? ''
  const uid = t.startsWith('mock.') ? t.slice(5) : null
  return uid ? (s.users[uid] ?? null) : null
}

export const viewOrder = (s: MockState, o: Order, viewerId: string): Order => ({
  ...o,
  handoff_code: viewerId === o.customer.id ? (s.codes[o.id] ?? null) : null,
  receipt_url: viewerId === o.customer.id || viewerId === o.runner.id ? o.receipt_url : null,
})

export const viewRun = (s: MockState, runId: string, viewerId: string): RunDetail | null => {
  const run = s.runs[runId]
  if (!run) return null
  const all = Object.values(s.orders).filter((o) => o.run_id === runId && o.status !== 'cancelled')
  const visible = run.runner.id === viewerId ? all : all.filter((o) => o.customer.id === viewerId)
  return { ...run, order_count: all.length, orders: visible.map((o) => viewOrder(s, o, viewerId)) }
}

export const circleMemberIds = (s: MockState, circleId: string) =>
  s.circles.find((c) => c.id === circleId)?.members.map((m) => m.id) ?? []

export const userCircles = (s: MockState, uid: string) => s.circles.filter((c) => c.members.some((m) => m.id === uid))

function runAudience(s: MockState, run: Run) {
  const customers = Object.values(s.orders).filter((o) => o.run_id === run.id).map((o) => o.customer.id)
  return [...new Set([...(run.status === 'open' ? Object.keys(s.users) : circleMemberIds(s, run.circle_id)), run.runner.id, ...customers])]
}

function emitRun(s: MockState, run: Run, emit: (e: StreamEvent, to: Audience) => void) {
  const all = Object.values(s.orders).filter((o) => o.run_id === run.id && o.status !== 'cancelled')
  emit({ type: 'run.updated', data: { run: { ...run, order_count: all.length } }, at: iso(Date.now()) }, runAudience(s, run))
}

function emitOrder(s: MockState, o: Order, emit: (e: StreamEvent, to: Audience) => void) {
  const at = iso(Date.now())
  emit({ type: 'order.updated', data: { order: viewOrder(s, o, o.customer.id) }, at }, [o.customer.id])
  emit({ type: 'order.updated', data: { order: viewOrder(s, o, o.runner.id) }, at }, [o.runner.id])
}

function addTxn(s: MockState, uid: string, t: Omit<WalletTxn, 'id' | 'at'>, now: number, emit: (e: StreamEvent, to: Audience) => void) {
  const w = s.wallets[uid]
  if (!w) return
  w.transactions.unshift({ ...t, id: id(s, 'txn_'), at: iso(now) })
  w.balance_cents += t.amount_cents
  emit({ type: 'wallet.updated', data: { wallet: w }, at: iso(now) }, [uid])
}

function notify(s: MockState, uid: string, n: Omit<Notification, 'id' | 'created_at' | 'read'>, now: number, emit: (e: StreamEvent, to: Audience) => void) {
  const full: Notification = { ...n, id: id(s, 'ntf_'), created_at: iso(now), read: false }
  ;(s.notifications[uid] ??= []).unshift(full)
  emit({ type: 'notification.created', data: { notification: full }, at: iso(now) }, [uid])
}

// ---------- orders ----------

export class MockError extends Error {
  status: number
  code: string
  constructor(status: number, code: string, message: string) {
    super(message)
    this.status = status
    this.code = code
  }
}

export function quoteOrderInternal(
  s: MockState,
  runId: string,
  lines: { menu_item_id: string; qty: number; choice_ids: string[]; note?: string }[],
  tip: number,
  now: number,
  skipChecks = false,
): { lines: Order['lines']; subtotal_cents: number; tax_cents: number; ferry_fee_cents: number; tip_cents: number; total_cents: number } {
  const run = s.runs[runId]
  if (!run) throw new MockError(404, 'not_found', 'That ferry doesn’t exist.')
  if (!skipChecks && (run.status !== 'open' || Date.parse(run.window_closes_at) < now))
    throw new MockError(409, 'window_closed', `${run.runner.display_name} already closed the window for this ferry.`)
  if (!lines.length) throw new MockError(422, 'validation_failed', 'Add at least one item.')
  const menu = menus[run.restaurant.id]
  const items = menu.categories.flatMap((c) => c.items)
  const outLines = lines.map((l) => {
    const item = items.find((i) => i.id === l.menu_item_id)
    if (!item) throw new MockError(422, 'validation_failed', 'One of those items isn’t on the menu.')
    const deltas = l.choice_ids.reduce((a, cid) => {
      const ch = item.options.flatMap((o) => o.choices).find((c) => c.id === cid)
      return a + (ch?.price_delta_cents ?? 0)
    }, 0)
    const unit = item.price_cents + deltas
    return { menu_item_id: item.id, name: item.name, qty: l.qty, choice_ids: l.choice_ids, unit_price_cents: unit, line_total_cents: unit * l.qty, ...(l.note ? { note: l.note } : {}) }
  })
  const subtotal = outLines.reduce((a, l) => a + l.line_total_cents, 0)
  const tax = Math.round(subtotal * TAX)
  const fee = ferryFeeCents()
  const total = subtotal + tax + fee + tip
  return { lines: outLines, subtotal_cents: subtotal, tax_cents: tax, ferry_fee_cents: fee, tip_cents: tip, total_cents: total }
}

export function placeOrderInternal(
  s: MockState,
  runId: string,
  customerId: string,
  lines: { menu_item_id: string; qty: number; choice_ids: string[]; note?: string }[],
  tip: number,
  dropoff: Place,
  now: number,
  emit: (e: StreamEvent, to: Audience) => void,
  skipChecks = false,
): Order {
  const run = s.runs[runId]
  const priced = quoteOrderInternal(s, runId, lines, tip, now, skipChecks)
  const total = priced.total_cents
  const wallet = s.wallets[customerId]
  if (!skipChecks && wallet && wallet.balance_cents < total)
    throw new MockError(402, 'insufficient_funds', 'Your Ferry Wallet doesn’t have enough for this order.')

  const oid = runId === 'run_demo' && customerId === 'usr_judge' ? 'ord_demo' : id(s, 'ord_')
  const order: Order = {
    id: oid,
    run_id: runId,
    restaurant_id: run.restaurant.id,
    customer: users[customerId] ?? { id: customerId, display_name: s.users[customerId]?.display_name ?? 'Guest', avatar_seed: customerId, bubble_color: '#1E8FD6' },
    runner: run.runner,
    lines: priced.lines,
    subtotal_cents: priced.subtotal_cents,
    tax_cents: priced.tax_cents,
    ferry_fee_cents: priced.ferry_fee_cents,
    tip_cents: tip,
    total_cents: total,
    status: 'funded',
    dropoff,
    handoff_code: null,
    eta_minutes: null,
    co2_saved_g: null,
    receipt_url: null,
    timeline: [{ status: 'funded', at: iso(now) }],
  }
  s.orders[oid] = order
  s.codes[oid] = String(1000 + Math.floor(Math.random() * 9000))
  if (!skipChecks) {
    addTxn(s, customerId, { kind: 'order_hold', amount_cents: -total, description: `Hold for your ${run.restaurant.name} order`, run_id: runId, order_id: oid }, now, emit)
    emitOrder(s, order, emit)
    emitRun(s, run, emit)
  }
  return order
}

function setOrderStatus(o: Order, status: OrderStatus, now: number) {
  o.status = status
  o.timeline = [...o.timeline.filter((t) => t.status !== status), { status, at: iso(now) }]
}

const runOrders = (s: MockState, runId: string) =>
  Object.values(s.orders).filter((o) => o.run_id === runId && !['cancelled', 'refunded'].includes(o.status))

export function closeRun(s: MockState, runId: string, now: number, emit: (e: StreamEvent, to: Audience) => void) {
  const run = s.runs[runId]
  if (run.status !== 'open') return run
  run.status = 'closed'
  run.window_closes_at = iso(Math.min(Date.parse(run.window_closes_at), now))
  const funding = runOrders(s, runId).reduce((a, o) => a + o.subtotal_cents + o.tax_cents, 0)
  addTxn(s, run.runner.id, { kind: 'card_funding', amount_cents: funding, description: `Escrow funded your Ferry Card for ${run.restaurant.name}`, run_id: runId }, now, emit)
  emitRun(s, run, emit)
  return run
}

export function markPurchased(s: MockState, runId: string, receiptTotal: number | null, receiptPath: string | undefined, now: number, emit: (e: StreamEvent, to: Audience) => void) {
  const run = s.runs[runId]
  if (run.status === 'open') closeRun(s, runId, now, emit)
  if (run.status !== 'closed') return run
  const orders = runOrders(s, runId)
  const estimate = orders.reduce((a, o) => a + o.subtotal_cents + o.tax_cents, 0)
  const total = receiptTotal ?? Math.round(estimate * 0.97)
  s.receiptTotals[runId] = total
  run.status = 'purchased'
  const receiptUrl = (receiptPath && s.receipts[receiptPath]) || '/mock/receipt.svg'
  for (const o of orders) {
    setOrderStatus(o, 'purchased', now)
    o.receipt_url = receiptUrl
    emitOrder(s, o, emit)
  }
  addTxn(s, run.runner.id, { kind: 'card_purchase', amount_cents: -total, description: `Ferry Card purchase at ${run.restaurant.name}`, run_id: runId }, now, emit)
  emitRun(s, run, emit)
  return run
}

export function markPickedUp(s: MockState, runId: string, now: number, emit: (e: StreamEvent, to: Audience) => void) {
  const run = s.runs[runId]
  if (run.status === 'closed') markPurchased(s, runId, null, undefined, now, emit)
  if (run.status !== 'purchased') return run
  run.status = 'delivering'
  run.runner_location = { location: { lat: run.restaurant.location.lat, lng: run.restaurant.location.lng }, at: iso(now) }
  run.eta_minutes = etaFrom(0)
  for (const o of runOrders(s, runId)) {
    setOrderStatus(o, 'picked_up', now)
    o.eta_minutes = run.eta_minutes
    emitOrder(s, o, emit)
    notify(s, o.customer.id, { type: 'order_update', title: `${run.runner.display_name} picked up your food`, body: `Walking to ${o.dropoff.label} now.`, data: { order_id: o.id } }, now, emit)
  }
  emitRun(s, run, emit)
  if (runId === 'run_demo') {
    s.sim.phase = 'walking'
    s.sim.walkStart = now
  }
  return run
}

export function handoff(s: MockState, orderId: string, viewerId: string, code: string | undefined, now: number, emit: (e: StreamEvent, to: Audience) => void) {
  const o = s.orders[orderId]
  if (!o) throw new MockError(404, 'not_found', 'Order not found.')
  if (viewerId === o.runner.id) {
    if (code !== s.codes[orderId]) throw new MockError(422, 'validation_failed', 'That code doesn’t match. Ask for the 4-digit code on their screen.')
  } else if (viewerId !== o.customer.id) throw new MockError(403, 'forbidden', 'Only the customer or captain can confirm.')
  if (!['picked_up', 'purchased'].includes(o.status)) return o
  settle(s, o, now, emit)
  const run = s.runs[o.run_id]
  if (runOrders(s, o.run_id).every((x) => ['delivered', 'settled'].includes(x.status))) {
    run.status = 'delivered'
    run.eta_minutes = 0
    emitRun(s, run, emit)
    if (o.run_id === 'run_demo') s.sim.phase = 'done'
  }
  if (o.run_id === 'run_demo' && o.customer.id === 'usr_judge' && !s.invites.inv_demo) s.sim.inviteAt = now + 9000
  return o
}

function settle(s: MockState, o: Order, now: number, emit: (e: StreamEvent, to: Audience) => void) {
  setOrderStatus(o, 'delivered', now)
  o.eta_minutes = 0
  o.co2_saved_g = CO2_PER_DELIVERY_G
  setOrderStatus(o, 'settled', now + 1)
  const run = s.runs[o.run_id]
  const estimate = runOrders(s, o.run_id).reduce((a, x) => a + x.subtotal_cents + x.tax_cents, 0)
  const receipt = s.receiptTotals[o.run_id] ?? estimate
  const share = Math.round(((o.subtotal_cents + o.tax_cents) / Math.max(estimate, 1)) * receipt)
  const refund = o.subtotal_cents + o.tax_cents - share
  if (refund > 0) addTxn(s, o.customer.id, { kind: 'refund', amount_cents: refund, description: 'Receipt came in under the estimate', run_id: o.run_id, order_id: o.id }, now, emit)
  addTxn(s, run.runner.id, { kind: 'payout', amount_cents: runnerPayCents(o.ferry_fee_cents, o.tip_cents), description: `Captain pay and tip from ${o.customer.display_name}`, run_id: o.run_id, order_id: o.id }, now, emit)
  emitOrder(s, o, emit)
  for (const c of s.circles) if (c.id === run.circle_id) c.stats.co2_saved_g += CO2_PER_DELIVERY_G
}

// ---------- walking ----------

const toRad = (d: number) => (d * Math.PI) / 180
export function meters(a: [number, number], b: [number, number]) {
  const R = 6371000
  const dLat = toRad(b[1] - a[1])
  const dLng = toRad(b[0] - a[0])
  const h = Math.sin(dLat / 2) ** 2 + Math.cos(toRad(a[1])) * Math.cos(toRad(b[1])) * Math.sin(dLng / 2) ** 2
  return 2 * R * Math.asin(Math.sqrt(h))
}

const segs = demoRoute.slice(1).map((p, i) => meters(demoRoute[i], p))
export const ROUTE_M = segs.reduce((a, b) => a + b, 0)

function pointAt(progress: number): [number, number] {
  let d = Math.min(Math.max(progress, 0), 1) * ROUTE_M
  for (let i = 0; i < segs.length; i++) {
    if (d <= segs[i]) {
      const t = segs[i] ? d / segs[i] : 0
      const [a, b] = [demoRoute[i], demoRoute[i + 1]]
      return [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
    }
    d -= segs[i]
  }
  return demoRoute[demoRoute.length - 1]
}

const etaFrom = (progress: number) => Math.max(0, Math.ceil(((1 - progress) * ROUTE_M) / WALK_SPEED_MPS / 60))

export function walkProgress(s: MockState, now: number) {
  if (s.sim.phase === 'walking') return Math.min(1, (now - s.sim.walkStart) / WALK_SIM_MS)
  if (s.sim.phase === 'arrived' || s.sim.phase === 'done') return 1
  return 0
}

export function trackFor(s: MockState, runId: string, now: number): Track {
  const run = s.runs[runId]
  const orders = runOrders(s, runId)
  const loc = run.runner_location
  if (runId !== 'run_demo') {
    return {
      runner_location: loc,
      route: null,
      distance_m: null,
      eta_minutes: run.eta_minutes,
      stops: orders.map((o) => ({ order_id: o.id, dropoff: o.dropoff, eta_minutes: run.eta_minutes ?? 0 })),
    }
  }
  const p = walkProgress(s, now)
  const delivering = run.status === 'delivering' || run.status === 'delivered'
  return {
    runner_location: loc,
    route: { type: 'LineString', coordinates: demoRoute },
    distance_m: delivering ? Math.round((1 - p) * ROUTE_M) : Math.round(ROUTE_M),
    eta_minutes: delivering ? etaFrom(p) : null,
    stops: orders.map((o) => ({ order_id: o.id, dropoff: o.dropoff, eta_minutes: delivering ? etaFrom(p) : etaFrom(0) + 8 })),
  }
}

export function setRunnerLocation(s: MockState, runId: string, location: LatLng, at: number, eta: number | null, emit: (e: StreamEvent, to: Audience) => void) {
  const run = s.runs[runId]
  run.runner_location = { location, at: iso(at) }
  run.eta_minutes = eta
  const to = [run.runner.id, ...runOrders(s, runId).map((o) => o.customer.id)]
  emit({ type: 'runner.location', data: { run_id: runId, location, at: iso(at), eta_minutes: eta }, at: iso(at) }, to)
  for (const o of runOrders(s, runId)) if (o.status === 'picked_up') o.eta_minutes = eta
}

// ---------- invites ----------

export function createDemoInvite(s: MockState, now: number, emit: (e: StreamEvent, to: Audience) => void) {
  if (s.invites.inv_demo) return s.invites.inv_demo
  const proposed = new Date(now + 864e5)
  proposed.setHours(19, 30, 0, 0)
  const inv: Invite = {
    id: 'inv_demo',
    kind: 'meetup',
    circle_id: 'cir_hinton4',
    from: users.usr_dana,
    message: 'Dinner at Lenoir again? Same crew as last week.',
    proposed_time: proposed.toISOString(),
    status: 'pending',
    responses: [
      { user: users.usr_dana, status: 'accepted' },
      { user: users.usr_maya, status: 'accepted' },
      { user: users.usr_judge, status: 'pending' },
      { user: users.usr_leo, status: 'pending' },
    ],
    thread: { state: 'none', compose: null, open_url: null },
  }
  s.invites.inv_demo = inv
  for (const uid of ['usr_judge', 'usr_maya', 'usr_leo']) {
    emit({ type: 'invite.updated', data: { invite: inv }, at: iso(now) }, [uid])
    notify(s, uid, { type: 'invite', title: 'Dana wants to meet again', body: inv.message, data: { invite_id: inv.id } }, now, emit)
  }
  return inv
}

export function respondInvite(s: MockState, inviteId: string, uid: string, accept: boolean, now: number, emit: (e: StreamEvent, to: Audience) => void) {
  const inv = s.invites[inviteId]
  if (!inv) throw new MockError(404, 'not_found', 'Invite not found.')
  inv.responses = inv.responses.map((r) => (r.user.id === uid ? { ...r, status: accept ? 'accepted' : 'declined' } : r))
  if (uid !== inv.from.id) inv.status = accept ? 'accepted' : inv.status
  if (accept && inv.thread.state === 'none') {
    inv.thread = { state: 'creating', compose: null, open_url: null }
    s.sim.threadReadyAt = now + 2500
  }
  emit({ type: 'invite.updated', data: { invite: inv }, at: iso(now) }, inv.responses.map((r) => r.user.id))
  return inv
}

function finishThread(s: MockState, now: number, emit: (e: StreamEvent, to: Audience) => void) {
  const inv = s.invites.inv_demo
  if (!inv) return
  // On Telegram the agent makes the group; compose (the iMessage fallback) is null.
  inv.thread = { state: 'ready', compose: null, open_url: 'https://t.me/+FerryMeetAgainDemo' }
  emit({ type: 'invite.updated', data: { invite: inv }, at: iso(now) }, inv.responses.map((r) => r.user.id))
}

// ---------- impact ----------

export function impactFor(s: MockState, scope: ImpactScope, uid: string | null, circleId?: string): Impact {
  const weeks = 8
  const base = scope === 'campus' ? [38, 51, 47, 63, 72, 69, 88, 0] : scope === 'circle' ? [1, 2, 1, 3, 2, 2, 3, 0] : [0, 1, 0, 1, 1, 0, 1, 0]
  const settledNow = Object.values(s.orders).filter((o) => o.status === 'settled' && Date.now() - Date.parse(o.timeline.at(-1)!.at) < 7 * 864e5)
  const mineNow = settledNow.filter((o) => {
    if (scope === 'campus') return true
    if (scope === 'me') return o.customer.id === uid || o.runner.id === uid
    const run = s.runs[o.run_id]
    return run?.circle_id === (circleId ?? 'cir_hinton4')
  }).length
  base[weeks - 1] = (scope === 'campus' ? 23 : 0) + mineNow
  // Contract: week_start is the week's Monday at 00:00 UTC, as a date-time.
  const monday = new Date()
  monday.setUTCHours(0, 0, 0, 0)
  monday.setUTCDate(monday.getUTCDate() - ((monday.getUTCDay() + 6) % 7))
  const by_week = base.map((d, i) => ({
    week_start: new Date(monday.getTime() - (weeks - 1 - i) * 7 * 864e5).toISOString(),
    deliveries: d,
    co2_saved_g: d * CO2_PER_DELIVERY_G,
  }))
  const deliveries = by_week.reduce((a, w) => a + w.deliveries, 0)
  return {
    scope,
    deliveries,
    car_miles_avoided: Math.round(deliveries * 1.2 * 10) / 10,
    co2_saved_g: deliveries * CO2_PER_DELIVERY_G,
    by_week,
    method: IMPACT_METHOD,
    includes_demo_history: true,
  }
}

// ---------- the demo clock ----------

export function tick(now = Date.now()) {
  mutate((s, emit) => {
    const sim = s.sim
    if (sim.tickOwner !== tabId && sim.tickUntil > now) return
    sim.tickOwner = tabId
    sim.tickUntil = now + 2500

    // Keep the demo ferry open while nobody has ordered yet.
    const demo = s.runs.run_demo
    let demoChanged = false
    if (demo.status === 'open' && sim.phase === 'idle' && Date.parse(demo.window_closes_at) < now + 90e3) {
      demo.window_closes_at = iso(now + 10 * 60e3)
      demo.leaving_at = iso(now + 4 * 60e3)
      demo.returning_at = iso(now + 32 * 60e3)
      demoChanged = true
    }
    if (demo.status === 'open' && sim.phase === 'idle' && demo.returning_at && Date.parse(demo.returning_at) <= Date.parse(demo.leaving_at)) {
      demo.returning_at = iso(Date.parse(demo.leaving_at) + 28 * 60e3)
      demoChanged = true
    }
    if (demoChanged) emitRun(s, demo, emit)
    for (const run of Object.values(s.runs)) {
      let changed = false
      if (run.id !== 'run_demo' && run.status === 'open' && Date.parse(run.window_closes_at) < now) {
        const returnGap = run.returning_at ? Date.parse(run.returning_at) - Date.parse(run.leaving_at) : 0
        run.window_closes_at = iso(now + 25 * 60e3)
        run.leaving_at = iso(now + 22 * 60e3)
        if (run.returning_at) run.returning_at = iso(now + 22 * 60e3 + (returnGap > 0 ? returnGap : 20 * 60e3))
        changed = true
      }
      if (run.id !== 'run_demo' && run.status === 'open' && run.returning_at && Date.parse(run.returning_at) <= Date.parse(run.leaving_at)) {
        run.returning_at = iso(Date.parse(run.leaving_at) + 20 * 60e3)
        changed = true
      }
      if (changed) emitRun(s, run, emit)
    }

    if (sim.auto && now >= sim.nextAt) {
      if (sim.phase === 'ordered') {
        closeRun(s, 'run_demo', now, emit)
        sim.phase = 'closed'
        sim.nextAt = now + 7000
      } else if (sim.phase === 'closed') {
        markPurchased(s, 'run_demo', null, undefined, now, emit)
        sim.phase = 'purchased'
        sim.nextAt = now + 6000
      } else if (sim.phase === 'purchased') {
        markPickedUp(s, 'run_demo', now, emit)
      }
    }
    if (sim.phase === 'walking' && !sim.realLocation) {
      const p = walkProgress(s, now)
      const [lng, lat] = pointAt(p)
      setRunnerLocation(s, 'run_demo', { lat, lng }, now, etaFrom(p), emit)
      if (p >= 1) {
        sim.phase = 'arrived'
        for (const o of runOrders(s, 'run_demo')) {
          if (o.customer.id !== 'usr_judge' && o.status === 'picked_up') settle(s, o, now, emit)
          else if (o.status === 'picked_up') notify(s, o.customer.id, { type: 'order_update', title: `${demo.runner.display_name} is here`, body: `Meet at ${o.dropoff.label}. Show your code.`, data: { order_id: o.id } }, now, emit)
        }
      }
    }
    if (sim.inviteAt && now >= sim.inviteAt) {
      sim.inviteAt = null
      createDemoInvite(s, now, emit)
    }
    if (sim.threadReadyAt && now >= sim.threadReadyAt) {
      sim.threadReadyAt = null
      finishThread(s, now, emit)
    }
  })
}

// Mock only: the user "tapped Start" in the Telegram bot for this challenge.
export function openChallenge(id: string) {
  mutate((s) => {
    const c = s.challenges[id]
    if (c && !c.opened) c.opened = Date.now()
  })
}

// Mock controls: jump the demo forward one step.
export function skipStep() {
  mutate((s, emit) => {
    const now = Date.now()
    const sim = s.sim
    sim.tickOwner = ''
    sim.tickUntil = 0
    if (sim.phase === 'walking') sim.walkStart = now - WALK_SIM_MS
    else if (sim.phase === 'idle') {
      if (!s.orders.ord_demo) {
        placeOrderInternal(s, 'run_demo', 'usr_judge', [{ menu_item_id: 'itm_cosmic_chicken_burrito', qty: 1, choice_ids: ['cho_cosmic_chicken_burrito_black_beans'] }], s.users.usr_judge.profile.default_tip_cents, places.place_fetzer, now, emit)
      }
      sim.phase = 'ordered'
      sim.auto = true
      sim.nextAt = now
    } else if (sim.phase === 'arrived' && s.orders.ord_demo) handoff(s, 'ord_demo', 'usr_judge', undefined, now, emit)
    else if (sim.phase === 'done' && !s.invites.inv_demo) sim.inviteAt = now
    else sim.nextAt = now
  })
  tick()
}

export function triggerInvite() {
  mutate((s, emit) => createDemoInvite(s, Date.now(), emit))
}
