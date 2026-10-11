import type { QueryClient } from '@tanstack/react-query'
import { env } from '../config/env'
import { api } from './endpoints'
import { whenReady } from './client'
import { keys, setWallet, upsertInvite, upsertOrder, upsertRun } from './hooks'
import type { Notification, RunDetail, StreamEvent, Track } from './types'

type Status = 'idle' | 'connecting' | 'open' | 'reconnecting'

// One WebSocket for the whole app. Reconnects with capped exponential backoff
// plus jitter, pings every 25 s, and upserts every event into the query cache
// by id, so a replayed or duplicated event changes nothing.
class Realtime {
  private ws: WebSocket | null = null
  private qc: QueryClient | null = null
  private token: string | null = null
  private attempt = 0
  private pingTimer: number | undefined
  private retryTimer: number | undefined
  // Bumped on every disconnect, so a ticket fetch that finishes late is dropped.
  private gen = 0
  private seenNotifications = new Set<string>()
  private statusListeners = new Set<(s: Status) => void>()
  private eventListeners = new Set<(e: StreamEvent) => void>()
  status: Status = 'idle'

  attach(qc: QueryClient) {
    this.qc = qc
  }

  connect(token: string | null) {
    if (token === this.token && this.ws) return
    this.disconnect()
    this.token = token
    if (!token) return
    void whenReady().then(() => this.token === token && !this.ws && void this.open())
  }

  disconnect() {
    this.gen++
    window.clearTimeout(this.retryTimer)
    window.clearInterval(this.pingTimer)
    if (this.ws) {
      this.ws.onclose = null
      this.ws.close()
    }
    this.ws = null
    this.token = null
    this.attempt = 0
    this.setStatus('idle')
  }

  onStatus(fn: (s: Status) => void) {
    this.statusListeners.add(fn)
    return () => this.statusListeners.delete(fn)
  }

  onEvent(fn: (e: StreamEvent) => void) {
    this.eventListeners.add(fn)
    return () => this.eventListeners.delete(fn)
  }

  private setStatus(s: Status) {
    this.status = s
    this.statusListeners.forEach((l) => l(s))
  }

  // A single-use ticket keeps the JWT out of the socket URL (contract v1.2);
  // fall back to the token if the ticket endpoint isn't there yet.
  private async streamUrl(token: string) {
    try {
      const { ticket } = await api.streamTicket()
      return `${env.wsBase}/v1/stream?ticket=${encodeURIComponent(ticket)}`
    } catch {
      return `${env.wsBase}/v1/stream?token=${encodeURIComponent(token)}`
    }
  }

  private async open() {
    if (!this.token) return
    const token = this.token
    const gen = this.gen
    this.setStatus(this.attempt ? 'reconnecting' : 'connecting')
    const url = await this.streamUrl(token)
    if (gen !== this.gen || this.token !== token || this.ws) return
    let ws: WebSocket
    try {
      ws = new WebSocket(url)
    } catch {
      this.scheduleRetry()
      return
    }
    this.ws = ws
    ws.onopen = () => {
      const wasReconnect = this.attempt > 0
      this.attempt = 0
      this.setStatus('open')
      window.clearInterval(this.pingTimer)
      this.pingTimer = window.setInterval(() => this.send({ type: 'ping' }), 25_000)
      // Catch up on anything missed while connecting or disconnected.
      if (wasReconnect) this.qc?.invalidateQueries()
      else {
        this.qc?.invalidateQueries({ queryKey: keys.notifications })
        this.qc?.invalidateQueries({ queryKey: keys.invites })
      }
    }
    ws.onmessage = (m) => {
      if (typeof m.data !== 'string') return
      let evt: StreamEvent
      try {
        evt = JSON.parse(m.data)
      } catch {
        return
      }
      this.apply(evt)
    }
    ws.onclose = () => {
      window.clearInterval(this.pingTimer)
      this.ws = null
      this.scheduleRetry()
    }
    ws.onerror = () => ws.close()
  }

  private scheduleRetry() {
    if (!this.token) return
    this.attempt++
    const base = Math.min(30_000, 1000 * 2 ** Math.min(this.attempt - 1, 5))
    const delay = base / 2 + Math.random() * (base / 2)
    this.setStatus('reconnecting')
    window.clearTimeout(this.retryTimer)
    this.retryTimer = window.setTimeout(() => void this.open(), delay)
  }

  private send(data: unknown) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(data))
  }

  private apply(evt: StreamEvent) {
    const qc = this.qc
    if (!qc || !evt?.type) return
    switch (evt.type) {
      case 'run.updated':
        upsertRun(qc, evt.data.run)
        break
      case 'order.updated':
        upsertOrder(qc, evt.data.order)
        if (evt.data.order.status === 'settled') qc.invalidateQueries({ queryKey: ['impact'] })
        break
      case 'runner.location': {
        // Keep only the newest position per run; late events are ignored.
        const { run_id, location, at, eta_minutes } = evt.data
        const newer = (cur: { at: string } | null | undefined) => !cur || Date.parse(at) >= Date.parse(cur.at)
        qc.setQueryData<RunDetail>(keys.run(run_id), (p) => (p && newer(p.runner_location) ? { ...p, runner_location: { location, at }, eta_minutes } : p))
        qc.setQueryData<Track>(keys.track(run_id), (p) => (p && newer(p.runner_location) ? { ...p, runner_location: { location, at }, eta_minutes } : p))
        break
      }
      case 'notification.created': {
        const n = evt.data.notification
        const fresh = !this.seenNotifications.has(n.id)
        this.seenNotifications.add(n.id)
        qc.setQueryData<Notification[]>(keys.notifications, (p) => {
          if (!p) return [n]
          return p.some((x) => x.id === n.id) ? p : [n, ...p]
        })
        if (!fresh) return
        break
      }
      case 'invite.updated':
        upsertInvite(qc, evt.data.invite)
        break
      case 'wallet.updated':
        setWallet(qc, evt.data.wallet)
        break
      default:
        return
    }
    this.eventListeners.forEach((l) => l(evt))
  }
}

export const realtime = new Realtime()
