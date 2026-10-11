import { useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query'
import { useSyncExternalStore } from 'react'
import { session } from '../state/session'
import { api } from './endpoints'
import type { ImpactScope, Invite, Order, Run, RunDetail, Wallet } from './types'

export const keys = {
  config: ['config'] as const,
  publicImpact: ['publicImpact'] as const,
  me: ['me'] as const,
  meStats: ['meStats'] as const,
  areas: ['areas'] as const,
  restaurants: (p: { area_id?: string; q?: string; open_now?: boolean }) => ['restaurants', p] as const,
  menu: (id: string) => ['menu', id] as const,
  places: ['places'] as const,
  openRuns: ['runs', 'open'] as const,
  myActiveRuns: ['runs', 'mine'] as const,
  run: (id: string) => ['run', id] as const,
  track: (id: string) => ['track', id] as const,
  myOrders: ['orders', 'mine'] as const,
  order: (id: string) => ['order', id] as const,
  circles: ['circles'] as const,
  circle: (id: string) => ['circle', id] as const,
  invites: ['invites'] as const,
  notifications: ['notifications'] as const,
  wallet: ['wallet'] as const,
  impact: (scope: ImpactScope, circleId?: string) => ['impact', scope, circleId ?? null] as const,
}

export function useToken() {
  return useSyncExternalStore(session.subscribe, () => session.token)
}

const STATIC = 5 * 60e3

export const useConfig = () => useQuery({ queryKey: keys.config, queryFn: api.config, staleTime: Infinity })
export const usePublicImpact = () => useQuery({ queryKey: keys.publicImpact, queryFn: api.publicImpact, staleTime: 60e3 })

export function useMe() {
  const token = useToken()
  return useQuery({ queryKey: keys.me, queryFn: api.me, enabled: !!token, staleTime: STATIC })
}
export const useMeStats = () => useQuery({ queryKey: keys.meStats, queryFn: api.meStats })
export const useAreas = () => useQuery({ queryKey: keys.areas, queryFn: api.areas, staleTime: STATIC })
export const useRestaurants = (p: { area_id?: string; q?: string; open_now?: boolean }) =>
  useQuery({ queryKey: keys.restaurants(p), queryFn: () => api.restaurants(p), staleTime: STATIC })
export const useMenu = (id: string | undefined) =>
  useQuery({ queryKey: keys.menu(id ?? ''), queryFn: () => api.menu(id!), enabled: !!id, staleTime: STATIC })
export const usePlaces = () => useQuery({ queryKey: keys.places, queryFn: api.places, staleTime: STATIC })
export const useMyActiveRuns = () => useQuery({ queryKey: keys.myActiveRuns, queryFn: api.myActiveRuns, refetchInterval: 60e3 })
export const useOpenRuns = () => useQuery({ queryKey: keys.openRuns, queryFn: api.openRuns, refetchInterval: 60e3 })
export const useRun = (id: string | undefined) =>
  useQuery({ queryKey: keys.run(id ?? ''), queryFn: () => api.run(id!), enabled: !!id })
export const useTrack = (id: string | undefined, active = true) =>
  useQuery({
    queryKey: keys.track(id ?? ''),
    queryFn: () => api.track(id!),
    enabled: !!id && active,
    // The stream carries live positions; this only refreshes the route and stops.
    refetchInterval: 15e3,
  })
export const useMyOrders = () => useQuery({ queryKey: keys.myOrders, queryFn: api.myOrders })
export const useOrder = (id: string | undefined) =>
  useQuery({ queryKey: keys.order(id ?? ''), queryFn: () => api.order(id!), enabled: !!id })
export const useCircles = () => useQuery({ queryKey: keys.circles, queryFn: api.circles })
export const useCircle = (id: string | undefined) =>
  useQuery({ queryKey: keys.circle(id ?? ''), queryFn: () => api.circle(id!), enabled: !!id })
export const useInvites = () => useQuery({ queryKey: keys.invites, queryFn: api.invites })
export const useNotifications = () => {
  const token = useToken()
  return useQuery({ queryKey: keys.notifications, queryFn: api.notifications, enabled: !!token })
}
export const useWallet = () => useQuery({ queryKey: keys.wallet, queryFn: api.wallet })
export const useImpact = (scope: ImpactScope, circleId?: string) =>
  useQuery({ queryKey: keys.impact(scope, circleId), queryFn: () => api.impact(scope, circleId) })

// ---------- cache upserts (shared by mutations and the stream) ----------

const upsertById = <T extends { id: string }>(list: T[] | undefined, item: T, prepend = false) => {
  if (!list) return list
  const i = list.findIndex((x) => x.id === item.id)
  if (i === -1) return prepend ? [item, ...list] : [...list, item]
  const next = list.slice()
  next[i] = item
  return next
}

export function upsertRun(qc: QueryClient, run: Run) {
  qc.setQueryData<RunDetail>(keys.run(run.id), (prev) => (prev ? { ...prev, ...run, orders: prev.orders } : prev))
  qc.setQueryData<Run[]>(keys.openRuns, (prev) => {
    if (!prev) return prev
    if (run.status !== 'open') return prev.filter((r) => r.id !== run.id)
    return upsertById(prev, run).sort((a, b) => a.window_closes_at.localeCompare(b.window_closes_at))
  })
}

export function upsertOrder(qc: QueryClient, order: Order) {
  qc.setQueryData(keys.order(order.id), order)
  qc.setQueryData<Order[]>(keys.myOrders, (prev) => {
    if (!prev) return prev
    // Only orders I placed belong in "mine"; a runner also receives others' orders.
    const me = qc.getQueryData<{ id: string }>(keys.me)
    if (me && order.customer.id !== me.id) return prev
    return upsertById(prev, order, true)
  })
  qc.setQueryData<RunDetail>(keys.run(order.run_id), (prev) =>
    prev ? { ...prev, orders: upsertById(prev.orders, order) ?? prev.orders } : prev,
  )
}

export function upsertInvite(qc: QueryClient, invite: Invite) {
  qc.setQueryData<Invite[]>(keys.invites, (prev) => (prev ? upsertById(prev, invite, true) : [invite]))
}

export function setWallet(qc: QueryClient, wallet: Wallet) {
  qc.setQueryData(keys.wallet, wallet)
}

// ---------- mutations ----------

export function usePlaceOrder(runId: string) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (body: Parameters<typeof api.placeOrder>[1]) => api.placeOrder(runId, body),
    onSuccess: (order) => {
      upsertOrder(qc, order)
      qc.invalidateQueries({ queryKey: keys.wallet })
      qc.invalidateQueries({ queryKey: keys.run(runId) })
    },
  })
}

export type RunAction =
  | { runId: string; action: 'close' | 'picked-up' | 'cancel' }
  | { runId: string; action: 'purchased'; receipt_total_cents: number; receipt_path?: string }

export function useRunAction() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: async (a: RunAction) => {
      switch (a.action) {
        case 'close':
          return api.closeRun(a.runId)
        case 'picked-up':
          return api.pickedUp(a.runId)
        case 'cancel':
          return api.cancelRun(a.runId)
        case 'purchased':
          return api.purchased(a.runId, { receipt_total_cents: a.receipt_total_cents, receipt_path: a.receipt_path })
      }
    },
    onSuccess: (run) => {
      upsertRun(qc, run)
      qc.invalidateQueries({ queryKey: keys.run(run.id) })
      qc.invalidateQueries({ queryKey: keys.wallet })
    },
  })
}

export function useHandoff() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (a: { orderId: string; code?: string }) => api.handoff(a.orderId, a.code),
    onSuccess: (order) => {
      upsertOrder(qc, order)
      qc.invalidateQueries({ queryKey: keys.wallet })
      qc.invalidateQueries({ queryKey: ['impact'] })
    },
  })
}

export function useRespondInvite() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (a: { id: string; accept: boolean }) => api.respondInvite(a.id, a.accept),
    onSuccess: (inv) => upsertInvite(qc, inv),
  })
}
