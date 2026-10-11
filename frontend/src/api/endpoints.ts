import { call, http } from './client'
import type { DropoffZone, RunPlan } from './pending'
import type { ImpactScope, LatLng, NewOrderLine, Persona, Place, Profile, Run, RunDetail, User } from './types'

const id = (v: string) => ({ params: { path: { id: v } } })

// One function per contract endpoint, all through the generated client.
export const api = {
  config: () => call(http.GET('/v1/public/config')),
  publicImpact: () => call(http.GET('/v1/public/impact')),
  pushKey: () => call(http.GET('/v1/public/push-key')),

  telegramStart: () => call(http.POST('/v1/auth/telegram/start')),
  telegramPoll: (challenge_id: string) => call(http.GET('/v1/auth/telegram/{challenge_id}', { params: { path: { challenge_id } } })),
  authDemo: (persona: Persona) => call(http.POST('/v1/auth/demo', { body: { persona } })),

  me: () => call(http.GET('/v1/me')) as Promise<User>,
  saveProfile: (body: { display_name?: string; profile: Profile }) => call(http.PUT('/v1/me/profile', { body })) as Promise<User>,
  meStats: () => call(http.GET('/v1/me/stats')),

  areas: () => call(http.GET('/v1/areas')),
  restaurants: (query: { area_id?: string; q?: string; open_now?: boolean }) => call(http.GET('/v1/restaurants', { params: { query } })),
  menu: (restaurantId: string) => call(http.GET('/v1/restaurants/{id}/menu', id(restaurantId))),

  places: () => call(http.GET('/v1/places')),
  openRuns: () => call(http.GET('/v1/runs', { params: { query: { status: 'open' } } })) as Promise<Run[]>,
  myActiveRuns: () => call(http.GET('/v1/runs', { params: { query: { mine: true } } })) as Promise<Run[]>,
  streamTicket: () => call(http.POST('/v1/stream/ticket')),
  createRun: (body: { restaurant_id: string; circle_id: string; leaving_at: string; window_minutes: number } & RunPlan) =>
    call(http.POST('/v1/runs', { body: body as never })) as Promise<RunDetail>,
  run: (runId: string) => call(http.GET('/v1/runs/{id}', id(runId))) as Promise<RunDetail>,
  placeOrder: (runId: string, body: { lines: NewOrderLine[]; tip_cents: number; dropoff: Place } & DropoffZone) =>
    call(http.POST('/v1/runs/{id}/orders', { ...id(runId), body: body as never })),
  quoteOrder: (runId: string, body: { lines: NewOrderLine[]; tip_cents: number; dropoff: Place } & DropoffZone) =>
    call(http.POST('/v1/runs/{id}/quote', { ...id(runId), body: body as never })),
  closeRun: (runId: string) => call(http.POST('/v1/runs/{id}/close', id(runId))),
  receiptUpload: (runId: string) => call(http.POST('/v1/runs/{id}/receipt-upload', id(runId))),
  purchased: (runId: string, body: { receipt_total_cents: number; receipt_path?: string }) =>
    call(http.POST('/v1/runs/{id}/purchased', { ...id(runId), body })),
  pickedUp: (runId: string) => call(http.POST('/v1/runs/{id}/picked-up', id(runId))),
  cancelRun: (runId: string) => call(http.POST('/v1/runs/{id}/cancel', id(runId))),
  postLocation: (runId: string, body: { location: LatLng; accuracy_m: number; at: string }) =>
    call(http.POST('/v1/runs/{id}/location', { ...id(runId), body })),
  track: (runId: string) => call(http.GET('/v1/runs/{id}/track', id(runId))),

  myOrders: () => call(http.GET('/v1/orders', { params: { query: { mine: true } } })),
  order: (orderId: string) => call(http.GET('/v1/orders/{id}', id(orderId))),
  handoff: (orderId: string, code?: string) => call(http.POST('/v1/orders/{id}/handoff', { ...id(orderId), body: code ? { code } : {} })),
  cancelOrder: (orderId: string) => call(http.POST('/v1/orders/{id}/cancel', id(orderId))),

  requestFerry: (body: { restaurant_id: string; lines: NewOrderLine[]; tip_cents: number; dropoff: Place; needed_by: string } & DropoffZone) =>
    call(http.POST('/v1/requests', { body: body as never })),

  circles: () => call(http.GET('/v1/circles')),
  circle: (circleId: string) => call(http.GET('/v1/circles/{id}', id(circleId))),
  meetAgain: (circleId: string, body: { message?: string; proposed_time?: string }) =>
    call(http.POST('/v1/circles/{id}/meet-again', { ...id(circleId), body })),

  invites: () => call(http.GET('/v1/invites')),
  respondInvite: (inviteId: string, accept: boolean) => call(http.POST('/v1/invites/{id}/respond', { ...id(inviteId), body: { accept } })),

  notifications: () => call(http.GET('/v1/notifications')),
  readNotification: (notificationId: string) => call(http.POST('/v1/notifications/{id}/read', id(notificationId))),
  pushSubscribe: (body: { platform: 'web' | 'ios' | 'android'; web_push?: PushSubscriptionJSON; token?: string }) =>
    call(http.POST('/v1/push/subscribe', { body: body as never })),

  wallet: () => call(http.GET('/v1/wallet')),
  demoTopup: (amount_cents: number) => call(http.POST('/v1/wallet/demo-topup', { body: { amount_cents } })),

  impact: (scope: ImpactScope, circle_id?: string) => call(http.GET('/v1/impact', { params: { query: { scope, circle_id } } })),
}
