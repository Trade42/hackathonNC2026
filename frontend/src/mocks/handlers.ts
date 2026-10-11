import { HttpResponse, delay, http, ws } from 'msw'
import { env } from '../config/env'
import type { RunPlan } from '../api/pending'
import type { Persona, PublicConfig } from '../api/types'
import { areas, menus, places, restaurants, users } from './fixtures'
import {
  MockError,
  closeRun,
  handoff,
  impactFor,
  load,
  markPickedUp,
  markPurchased,
  mutate,
  onEvent,
  placeOrderInternal,
  quoteOrderInternal,
  respondInvite,
  setRunnerLocation,
  trackFor,
  userCircles,
  userFromToken,
  viewOrder,
  viewRun,
  type MockState,
} from './db'

const API = `${env.apiBase}/v1`
// Placeholder bot until the backend names the real one in PublicConfig.
const TELEGRAM_BOT = 'https://t.me/FerryHackNCBot'

export const MOCK_SUPABASE_URL = 'https://ferry-mock.supabase.co'

const err = (status: number, code: string, message: string) =>
  HttpResponse.json({ error: { code, message } }, { status })

// Small latency so loading states are real, without slowing the demo.
const lag = () => delay(120 + Math.random() * 160)

type Ctx = { s: MockState; uid: string; request: Request; params: Record<string, string | readonly string[] | undefined> }

function authed(fn: (ctx: Ctx) => Response | Promise<Response>) {
  return async ({ request, params }: { request: Request; params: Record<string, string | readonly string[] | undefined> }) => {
    await lag()
    const s = load()
    const user = userFromToken(s, request.headers.get('Authorization'))
    if (!user) return err(401, 'unauthorized', 'Sign in to continue.')
    try {
      return await fn({ s, uid: user.id, request, params })
    } catch (e) {
      if (e instanceof MockError) return err(e.status, e.code, e.message)
      throw e
    }
  }
}

const p = (v: string | readonly string[] | undefined) => String(v ?? '')

const config = (): PublicConfig => ({
  web_url: typeof location !== 'undefined' ? location.origin : 'https://ferry.app',
  demo_mode: true,
  agent: { imessage_phone_e164: null, display: 'Ferry', telegram_url: TELEGRAM_BOT, telegram_add_to_group_url: `${TELEGRAM_BOT}?startgroup=ferry`, whatsapp_url: null },
  downloads: { macos: null, android: typeof location !== 'undefined' ? `${location.origin}/downloads/ferry.apk` : null, ios: null },
  supabase: { url: MOCK_SUPABASE_URL, publishable_key: 'sb_publishable_mock_key' },
})

export const handlers = [
  http.get(`${API}/public/config`, async () => {
    await lag()
    return HttpResponse.json(config())
  }),
  http.get(`${API}/public/impact`, async () => {
    await lag()
    return HttpResponse.json(impactFor(load(), 'campus', null))
  }),
  http.get(`${API}/public/push-key`, () => HttpResponse.json({ vapid_public_key: 'BMockVapidKeyForFerryDemoOnly' })),

  // Telegram sign-in: in mock mode the tap stands in for Start and Confirm.
  http.post(`${API}/auth/telegram/start`, async () => {
    await lag()
    const challenge_id = mutate((s) => {
      const cid = `chl_${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`
      s.challenges[cid] = { created: Date.now(), userId: 'usr_guest', isNew: true }
      return cid
    })
    return HttpResponse.json({
      challenge_id,
      bot_url: `${TELEGRAM_BOT}?start=${challenge_id}`,
      expires_at: new Date(Date.now() + 10 * 60e3).toISOString(),
      confirm_code: '4821',
    })
  }),
  http.get(`${API}/auth/telegram/:id`, async ({ params }) => {
    await lag()
    return mutate((s) => {
      const c = s.challenges[p(params.id)]
      if (!c) return err(404, 'not_found', 'That sign-in link expired. Start again.')
      if (Date.now() - c.created > 10 * 60e3) return HttpResponse.json({ status: 'expired' })
      if (!c.opened || Date.now() - c.opened < 3000) return HttpResponse.json({ status: 'pending' })
      if (!s.users.usr_guest) {
        s.users.usr_guest = {
          id: 'usr_guest',
          display_name: '',
          avatar_seed: 'guest-1',
          bubble_color: '#0C7A8A',
          phone_masked: null,
          telegram_username: 'new_ferry_student',
          onboarding_complete: false,
          profile: { ...s.users.usr_judge.profile, dropoff_default: null, favorite_cuisines: [], favorite_restaurant_ids: [], usual_times: [] },
        }
        s.wallets.usr_guest = { balance_cents: 2500, account_label: 'Nessie checking ending 0199', mode: 'nessie_mock', transactions: [{ id: 'txn_g1', kind: 'deposit', amount_cents: 2500, description: 'Sandbox deposit from Capital One Nessie', at: new Date().toISOString() }] }
      }
      const user = s.users.usr_guest
      return HttpResponse.json({ status: 'verified', token: 'mock.usr_guest', user, is_new: !user.onboarding_complete })
    })
  }),
  http.post(`${API}/auth/demo`, async ({ request }) => {
    await lag()
    const { persona } = (await request.json()) as { persona: Persona }
    const uid = `usr_${persona}`
    const s = load()
    if (!s.users[uid]) return err(422, 'validation_failed', 'Unknown persona.')
    return HttpResponse.json({ token: `mock.${uid}`, user: s.users[uid] })
  }),

  http.get(`${API}/places`, authed(() => HttpResponse.json(Object.values(places)))),
  http.post(
    `${API}/stream/ticket`,
    authed(({ uid }) => HttpResponse.json({ ticket: `tkt.${uid}.${Math.random().toString(36).slice(2, 10)}`, expires_at: new Date(Date.now() + 60e3).toISOString() })),
  ),

  http.get(`${API}/me`, authed(({ s, uid }) => HttpResponse.json(s.users[uid]))),
  http.put(
    `${API}/me/profile`,
    authed(async ({ uid, request }) => {
      const body = (await request.json()) as { display_name?: string; profile: MockState['users'][string]['profile'] }
      const user = mutate((s) => {
        const u = s.users[uid]
        if (body.display_name) {
          u.display_name = body.display_name
          for (const c of s.circles) c.members = c.members.map((m) => (m.id === uid ? { ...m, display_name: body.display_name! } : m))
        }
        u.profile = body.profile
        u.onboarding_complete = true
        return u
      })
      return HttpResponse.json(user)
    }),
  ),
  http.get(
    `${API}/me/stats`,
    authed(({ s, uid }) => {
      const mine = Object.values(s.orders).filter((o) => o.customer.id === uid)
      const ran = Object.values(s.runs).filter((r) => r.runner.id === uid)
      const earned = (s.wallets[uid]?.transactions ?? []).filter((t) => t.kind === 'payout').reduce((a, t) => a + t.amount_cents, 0)
      const co2 = Object.values(s.orders).filter((o) => (o.customer.id === uid || o.runner.id === uid) && o.co2_saved_g).reduce((a, o) => a + (o.co2_saved_g ?? 0), 0)
      return HttpResponse.json({ orders: mine.length, runs: ran.length, earned_cents: earned, co2_saved_g: co2 })
    }),
  ),

  http.get(`${API}/areas`, authed(() => HttpResponse.json(areas))),
  http.get(
    `${API}/restaurants`,
    authed(({ request }) => {
      const u = new URL(request.url)
      const area = u.searchParams.get('area_id')
      const q = u.searchParams.get('q')?.toLowerCase()
      const openNow = u.searchParams.get('open_now') === 'true'
      const list = restaurants.filter(
        (r) =>
          (!area || r.area_id === area) &&
          (!openNow || r.is_open) &&
          (!q || r.name.toLowerCase().includes(q) || r.cuisine.some((c) => c.toLowerCase().includes(q))),
      )
      return HttpResponse.json(list)
    }),
  ),
  http.get(
    `${API}/restaurants/:id/menu`,
    authed(({ params }) => {
      const m = menus[p(params.id)]
      return m ? HttpResponse.json(m) : err(404, 'not_found', 'Menu not found.')
    }),
  ),

  http.get(
    `${API}/runs`,
    authed(({ s, uid, request }) => {
      // ?mine=true: my own active runs as runner, in any circle (contract v1.2).
      const mine = new URL(request.url).searchParams.get('mine') === 'true'
      const ACTIVE_RUN = ['open', 'closed', 'purchased', 'delivering']
      const list = Object.values(s.runs)
        .filter((r) => (mine ? r.runner.id === uid && ACTIVE_RUN.includes(r.status) : r.status === 'open'))
        .map((r) => {
          const { orders: _o, ...run } = viewRun(s, r.id, uid)!
          void _o
          return run
        })
        .sort((a, b) => a.window_closes_at.localeCompare(b.window_closes_at))
      return HttpResponse.json(list)
    }),
  ),
  http.post(
    `${API}/runs`,
    authed(async ({ uid, request }) => {
      const body = (await request.json()) as { restaurant_id: string; circle_id: string; leaving_at: string; window_minutes: number } & RunPlan
      const rest = restaurants.find((r) => r.id === body.restaurant_id)
      if (!rest) return err(422, 'validation_failed', 'Pick a restaurant.')
      const run = mutate((s, emit) => {
        const rid = `run_${Date.now().toString(36)}`
        const leaving = Date.parse(body.leaving_at)
        s.runs[rid] = {
          id: rid,
          restaurant: rest,
          runner: users[uid] ?? { id: uid, display_name: s.users[uid].display_name, avatar_seed: s.users[uid].avatar_seed, bubble_color: s.users[uid].bubble_color },
          circle_id: body.circle_id,
          status: 'open',
          leaving_at: body.leaving_at,
          window_closes_at: new Date(leaving + body.window_minutes * 60e3).toISOString(),
          order_count: 0,
          created_via: 'app',
          runner_location: null,
          eta_minutes: null,
          returning_at: body.returning_at ?? null,
          return_place: body.return_place ?? null,
          dropoff_radius_m: body.dropoff_radius_m ?? null,
        }
        emit({ type: 'run.updated', data: { run: s.runs[rid] }, at: new Date().toISOString() }, Object.keys(s.users))
        return s.runs[rid]
      })
      return HttpResponse.json(run, { status: 201 })
    }),
  ),
  http.get(
    `${API}/runs/:id`,
    authed(({ s, uid, params }) => {
      const run = viewRun(s, p(params.id), uid)
      return run ? HttpResponse.json(run) : err(404, 'not_found', 'That ferry doesn’t exist anymore.')
    }),
  ),
  http.post(
    `${API}/runs/:id/quote`,
    authed(async ({ s, request, params }) => {
      const body = (await request.json()) as { lines: { menu_item_id: string; qty: number; choice_ids: string[]; note?: string }[]; tip_cents: number }
      const { lines: _lines, ...quote } = quoteOrderInternal(s, p(params.id), body.lines, body.tip_cents, Date.now())
      void _lines
      return HttpResponse.json(quote)
    }),
  ),
  http.post(
    `${API}/runs/:id/orders`,
    authed(async ({ uid, request, params }) => {
      const body = (await request.json()) as { lines: { menu_item_id: string; qty: number; choice_ids: string[]; note?: string }[]; tip_cents: number; dropoff: MockState['orders'][string]['dropoff'] }
      const runId = p(params.id)
      const order = mutate((s, emit) => {
        const o = placeOrderInternal(s, runId, uid, body.lines, body.tip_cents, body.dropoff, Date.now(), emit)
        if (runId === 'run_demo' && uid !== s.runs.run_demo.runner.id && s.sim.phase === 'idle') {
          s.sim.phase = 'ordered'
          s.sim.auto = true
          s.sim.nextAt = Date.now() + 12000
        }
        return viewOrder(s, o, uid)
      })
      return HttpResponse.json(order, { status: 201 })
    }),
  ),
  ...(['close', 'purchased', 'picked-up', 'cancel'] as const).map((action) =>
    http.post(
      `${API}/runs/:id/${action}`,
      authed(async ({ s, uid, request, params }) => {
        const runId = p(params.id)
        const run = s.runs[runId]
        if (!run) return err(404, 'not_found', 'That ferry doesn’t exist.')
        if (run.runner.id !== uid) return err(403, 'forbidden', 'Only the captain can do that.')
        const body = action === 'purchased' ? ((await request.json()) as { receipt_total_cents: number; receipt_path?: string }) : null
        const updated = mutate((st, emit) => {
          const now = Date.now()
          if (runId === 'run_demo') st.sim.auto = false
          if (action === 'close') return closeRun(st, runId, now, emit)
          if (action === 'purchased') return markPurchased(st, runId, body!.receipt_total_cents, body!.receipt_path, now, emit)
          if (action === 'picked-up') return markPickedUp(st, runId, now, emit)
          st.runs[runId].status = 'cancelled'
          return st.runs[runId]
        })
        return HttpResponse.json(updated)
      }),
    ),
  ),
  http.post(
    `${API}/runs/:id/receipt-upload`,
    authed(({ params }) =>
      HttpResponse.json({
        bucket: 'receipts',
        path: `${p(params.id)}/${Date.now()}.jpg`,
        token: 'mock-upload-token',
        expires_at: new Date(Date.now() + 2 * 60e3).toISOString(),
      }),
    ),
  ),
  http.post(
    `${API}/runs/:id/location`,
    authed(async ({ uid, request, params }) => {
      const body = (await request.json()) as { location: { lat: number; lng: number }; at: string }
      mutate((s, emit) => {
        const run = s.runs[p(params.id)]
        if (!run || run.runner.id !== uid || run.status !== 'delivering') return
        // A real phone is reporting: stop the scripted walk.
        if (run.id === 'run_demo') s.sim.realLocation = true
        const eta = run.eta_minutes
        setRunnerLocation(s, run.id, body.location, Date.parse(body.at) || Date.now(), eta, emit)
      })
      return new HttpResponse(null, { status: 204 })
    }),
  ),
  http.get(
    `${API}/runs/:id/track`,
    authed(({ s, params }) => {
      const id = p(params.id)
      return s.runs[id] ? HttpResponse.json(trackFor(s, id, Date.now())) : err(404, 'not_found', 'Not found.')
    }),
  ),

  http.get(
    `${API}/orders`,
    authed(({ s, uid }) =>
      HttpResponse.json(
        Object.values(s.orders)
          .filter((o) => o.customer.id === uid)
          .sort((a, b) => b.timeline[0].at.localeCompare(a.timeline[0].at))
          .map((o) => viewOrder(s, o, uid)),
      ),
    ),
  ),
  http.get(
    `${API}/orders/:id`,
    authed(({ s, uid, params }) => {
      const o = s.orders[p(params.id)]
      if (!o) return err(404, 'not_found', 'Order not found.')
      if (o.customer.id !== uid && o.runner.id !== uid) return err(403, 'forbidden', 'That order belongs to someone else.')
      return HttpResponse.json(viewOrder(s, o, uid))
    }),
  ),
  http.post(
    `${API}/orders/:id/handoff`,
    authed(async ({ uid, request, params }) => {
      const body = ((await request.json().catch(() => ({}))) ?? {}) as { code?: string }
      const o = mutate((s, emit) => viewOrder(s, handoff(s, p(params.id), uid, body.code, Date.now(), emit), uid))
      return HttpResponse.json(o)
    }),
  ),
  http.post(
    `${API}/orders/:id/cancel`,
    authed(({ uid, params }) => {
      const o = mutate((s, emit) => {
        const order = s.orders[p(params.id)]
        if (!order) throw new MockError(404, 'not_found', 'Order not found.')
        if (order.status !== 'funded') throw new MockError(409, 'conflict', 'The captain already bought this order.')
        order.status = 'refunded'
        order.timeline.push({ status: 'refunded', at: new Date().toISOString() })
        const w = s.wallets[uid]
        w.balance_cents += order.total_cents
        w.transactions.unshift({ id: `txn_${Date.now()}`, kind: 'refund', amount_cents: order.total_cents, description: 'Order cancelled, hold released', at: new Date().toISOString(), run_id: order.run_id, order_id: order.id })
        emit({ type: 'wallet.updated', data: { wallet: w }, at: new Date().toISOString() }, [uid])
        return viewOrder(s, order, uid)
      })
      return HttpResponse.json(o)
    }),
  ),

  http.post(
    `${API}/requests`,
    authed(() => HttpResponse.json({ id: `req_${Date.now().toString(36)}`, status: 'searching', run_id: null }, { status: 201 })),
  ),

  http.get(`${API}/circles`, authed(({ s, uid }) => HttpResponse.json(userCircles(s, uid)))),
  http.get(
    `${API}/circles/:id`,
    authed(({ s, params }) => {
      const c = s.circles.find((x) => x.id === p(params.id))
      return c ? HttpResponse.json(c) : err(404, 'not_found', 'Circle not found.')
    }),
  ),
  http.post(
    `${API}/circles/:id/meet-again`,
    authed(async ({ uid, request, params }) => {
      const body = (await request.json()) as { message?: string; proposed_time?: string }
      const inv = mutate((s, emit) => {
        const c = s.circles.find((x) => x.id === p(params.id))
        if (!c) throw new MockError(404, 'not_found', 'Circle not found.')
        const from = c.members.find((m) => m.id === uid)!
        const iid = `inv_${Date.now().toString(36)}`
        s.invites[iid] = {
          id: iid,
          kind: 'meetup',
          circle_id: c.id,
          from,
          message: body.message || `Meet again? Same crew as last time.`,
          proposed_time: body.proposed_time ?? null,
          status: 'pending',
          responses: c.members.map((m) => ({ user: m, status: m.id === uid ? 'accepted' : 'pending' })),
          thread: { state: 'none', compose: null, open_url: null },
        }
        emit({ type: 'invite.updated', data: { invite: s.invites[iid] }, at: new Date().toISOString() }, c.members.map((m) => m.id))
        return s.invites[iid]
      })
      return HttpResponse.json(inv, { status: 201 })
    }),
  ),
  http.get(
    `${API}/invites`,
    authed(({ s, uid }) => HttpResponse.json(Object.values(s.invites).filter((i) => i.responses.some((r) => r.user.id === uid)))),
  ),
  http.post(
    `${API}/invites/:id/respond`,
    authed(async ({ uid, request, params }) => {
      const { accept } = (await request.json()) as { accept: boolean }
      return HttpResponse.json(mutate((s, emit) => respondInvite(s, p(params.id), uid, accept, Date.now(), emit)))
    }),
  ),

  http.get(`${API}/notifications`, authed(({ s, uid }) => HttpResponse.json(s.notifications[uid] ?? []))),
  http.post(
    `${API}/notifications/:id/read`,
    authed(({ uid, params }) => {
      mutate((s) => {
        for (const n of s.notifications[uid] ?? []) if (n.id === p(params.id)) n.read = true
      })
      return new HttpResponse(null, { status: 204 })
    }),
  ),
  http.post(`${API}/push/subscribe`, authed(() => new HttpResponse(null, { status: 204 }))),

  http.get(`${API}/wallet`, authed(({ s, uid }) => HttpResponse.json(s.wallets[uid]))),
  http.post(
    `${API}/wallet/demo-topup`,
    authed(async ({ uid, request }) => {
      const { amount_cents } = (await request.json()) as { amount_cents: number }
      const w = mutate((s, emit) => {
        const wallet = s.wallets[uid]
        wallet.balance_cents += amount_cents
        wallet.transactions.unshift({ id: `txn_${Date.now()}`, kind: 'deposit', amount_cents, description: 'Demo top-up from Capital One Nessie', at: new Date().toISOString() })
        emit({ type: 'wallet.updated', data: { wallet }, at: new Date().toISOString() }, [uid])
        return wallet
      })
      return HttpResponse.json(w)
    }),
  ),

  http.get(
    `${API}/impact`,
    authed(({ s, uid, request }) => {
      const u = new URL(request.url)
      const scope = (u.searchParams.get('scope') ?? 'me') as 'me' | 'circle' | 'campus'
      return HttpResponse.json(impactFor(s, scope, uid, u.searchParams.get('circle_id') ?? undefined))
    }),
  ),

  // Supabase Storage signed upload (what supabase-js uploadToSignedUrl calls).
  http.put(`${MOCK_SUPABASE_URL}/storage/v1/object/upload/sign/:bucket/*`, async ({ request, params }) => {
    const blob = await request.blob()
    const path = decodeURIComponent(new URL(request.url).pathname.split(`/upload/sign/${p(params.bucket)}/`)[1] ?? '')
    const dataUrl = await blobToDataUrl(blob)
    mutate((s) => {
      s.receipts[path] = dataUrl
    })
    return HttpResponse.json({ Key: `${p(params.bucket)}/${path}` })
  }),
]

async function blobToDataUrl(blob: Blob) {
  // supabase-js may send FormData; pull the file out if so.
  if (blob.type.startsWith('multipart/')) {
    return '/mock/receipt.svg'
  }
  return new Promise<string>((resolve) => {
    const fr = new FileReader()
    fr.onload = () => resolve(String(fr.result))
    fr.onerror = () => resolve('/mock/receipt.svg')
    fr.readAsDataURL(blob)
  })
}

// Live events over the mock WebSocket, filtered to each connected user.
const stream = ws.link(`${env.wsBase}/v1/stream`)
const clientUser = new Map<string, string>()

export const wsHandler = stream.addEventListener('connection', ({ client }) => {
  const q = new URL(client.url).searchParams
  const token = q.get('token') ?? ''
  const ticket = q.get('ticket') ?? ''
  const uid = token.startsWith('mock.') ? token.slice(5) : ticket.startsWith('tkt.') ? ticket.split('.')[1] : ''
  clientUser.set(client.id, uid)
  // Contract: the server sends nothing back for a ping.
  client.addEventListener('close', () => clientUser.delete(client.id))
})

onEvent(({ event, to }) => {
  for (const c of stream.clients) {
    const uid = clientUser.get(c.id)
    if (uid && to.includes(uid)) c.send(JSON.stringify(event))
  }
})
