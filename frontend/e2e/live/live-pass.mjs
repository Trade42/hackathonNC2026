// Live pass: drives the built web app against the live API at a phone size
// and checks every REST response and stream event it sees against
// contract/openapi.yaml. Ask for a demo reset before and after each run.
//
//   npm run build && npm run preview:live   (in one shell)
//   npm run test:live                       (in another)
//
// BASE (default http://localhost:5173) is the app; API is read from .env.
// ONLY=<regex> runs just the steps whose names match (e.g. ONLY='sign-in|meet-again').
// Writes e2e/live/out/results.json and screenshots; exits 1 on any red step.
//
// Real Telegram: the judge persona (Jordan) is linked to a real Telegram
// account. A full run sends it the order notices: "Order placed", "Maya bought
// your food", "Maya picked up your food" and "Delivered". Meet-again runs in
// cir_climb (Maya and Leo only, no Telegram-linked member) so its invite
// reaches nobody real.
import { chromium } from '@playwright/test'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { validator } from './contract.mjs'

const base = process.env.BASE ?? 'http://localhost:5173'
const dotenv = Object.fromEntries(
  readFileSync(new URL('../../.env', import.meta.url), 'utf8')
    .split('\n')
    .filter((l) => /^[A-Z_]+=/.test(l))
    .map((l) => l.split(/=(.*)/s).slice(0, 2)),
)
const API = (process.env.API ?? dotenv.VITE_API_BASE_URL).replace(/\/+$/, '')
const out = new URL('./out/', import.meta.url).pathname
mkdirSync(out, { recursive: true })

const check = await validator()
const t0 = Date.now()
const log = (s) => console.log(`${((Date.now() - t0) / 1000).toFixed(1)}s ${s}`)
const steps = []
const traffic = []
const events = []

const browser = await chromium.launch()

async function persona(name, label) {
  const ctx = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true })
  const page = await ctx.newPage()
  page.on('pageerror', (e) => traffic.push({ who: name, pageerror: e.message }))
  page.on('response', async (r) => {
    const url = r.url()
    if (!url.startsWith(API) || r.request().method() === 'OPTIONS') return
    const path = new URL(url).pathname
    let body = null
    try {
      body = r.status() === 204 ? null : await r.json()
    } catch {
      body = '(not json)'
    }
    const problems = check.response(r.request().method(), path, r.status(), body)
    traffic.push({ who: name, method: r.request().method(), path, status: r.status(), problems, body })
  })
  page.on('websocket', (ws) => {
    ws.on('framereceived', ({ payload }) => {
      if (typeof payload !== 'string') return
      let evt
      try {
        evt = JSON.parse(payload)
      } catch {
        return
      }
      events.push({ who: name, type: evt.type, problems: check.event(evt), evt })
    })
  })
  await page.goto(base + '/')
  await page.evaluate(() => {
    localStorage.clear()
    sessionStorage.clear()
  })
  return { name, label, ctx, page }
}

async function signIn(p, next) {
  await p.page.goto(`${base}/signin?next=${encodeURIComponent(next)}`)
  await p.page.getByRole('button', { name: new RegExp(`Continue as ${p.label}`) }).click()
  await p.page.waitForURL((u) => u.pathname === next, { timeout: 20_000 })
}

// Waits for the app's own call to an endpoint, so each step records what the UI sent.
const waitApi = (p, method, re, timeout = 20_000) =>
  p.page.waitForResponse((r) => r.url().startsWith(API) && r.request().method() === method && re.test(new URL(r.url()).pathname), { timeout })

// Calls the API from inside the page (same origin, CORS and token as the app).
const fetchAs = (p, method, path, body) =>
  p.page.evaluate(
    async ({ api, method, path, body }) => {
      const r = await fetch(api + path, {
        method,
        headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${localStorage.getItem('ferry.token')}` },
        body: body ? JSON.stringify(body) : undefined,
      })
      return { status: r.status, body: await r.json().catch(() => null) }
    },
    { api: API, method, path, body },
  )

async function step(name, screen, fn) {
  if (process.env.ONLY && !new RegExp(process.env.ONLY).test(name)) return
  const from = traffic.length
  const s = { step: name, screen, result: 'green', note: '' }
  try {
    s.note = (await fn()) ?? ''
  } catch (e) {
    s.result = 'red'
    s.note = String(e.message ?? e).split('\n')[0]
  }
  await new Promise((r) => setTimeout(r, 800)) // let this step's last responses land
  s.calls = traffic
    .slice(from)
    .filter((t) => t.method)
    .map((t) => `${t.method} ${t.path} ${t.status}${t.problems.length ? ' (contract: ' + t.problems.join('; ') + ')' : ''}`)
  const bad = traffic.slice(from).filter((t) => (t.problems?.length || t.pageerror) && !t.status?.toString().startsWith('4'))
  if (s.result === 'green' && bad.length) s.result = 'red'
  steps.push(s)
  log(`${s.result.toUpperCase()} ${name}${s.note ? ' — ' + s.note : ''}`)
}

const judge = await persona('judge', 'Jordan')
const maya = await persona('maya', 'Maya')
const dana = await persona('dana', 'Dana')
const leo = await persona('leo', 'Leo')
const money = (c) => `$${(c / 100).toFixed(2)}`
let quote = null
let order = null

await step('demo sign-in: judge, Maya, Dana, Leo', '/signin', async () => {
  await signIn(judge, '/home')
  await signIn(maya, '/home')
  await signIn(dana, '/home')
  await signIn(leo, '/home')
})

await step('judge orders on run_demo (quote, then place)', '/r/run_demo → /r/run_demo/checkout', async () => {
  await judge.page.goto(`${base}/r/run_demo`)
  await judge.page.getByRole('button', { name: /Chicken Burrito/ }).first().click()
  await judge.page.getByRole('button', { name: /Add to order/ }).click()
  const q = waitApi(judge, 'POST', /\/v1\/runs\/run_demo\/quote$/)
  await judge.page.locator('#cart-bar').click()
  await judge.page.waitForURL(/checkout/)
  quote = await (await q).json()
  await judge.page.waitForTimeout(1000)
  const shown = await judge.page.locator('main').innerText()
  await judge.page.screenshot({ path: out + 'checkout.png', fullPage: true })
  writeFileSync(out + 'checkout.txt', shown)
  const placed = waitApi(judge, 'POST', /\/v1\/runs\/run_demo\/orders$/)
  await judge.page.getByRole('button', { name: /^Place order/ }).click()
  const pr = await placed
  if (!pr.ok()) throw new Error(`place order ${pr.status()} ${await pr.text()}`)
  order = (await pr.json()).order ?? (await pr.json())
  await judge.page.waitForURL(/\/o\//)
  return `quote total ${money(quote.total_cents)}; order ${order.id} ${order.status} ${money(order.total_cents)}`
})

await step('Maya closes the window', '/run/run_demo', async () => {
  await maya.page.goto(`${base}/run/run_demo`)
  const r = waitApi(maya, 'POST', /\/close$/)
  await maya.page.getByRole('button', { name: 'Close window' }).click()
  const res = await r
  if (!res.ok()) throw new Error(`close ${res.status()} ${await res.text()}`)
})

await step('Maya uploads a receipt and marks it purchased', '/run/run_demo', async () => {
  const total = order ? (order.total_cents - (order.tip_cents ?? 0) - (order.ferry_fee_cents ?? 0)) : 1200
  await maya.page.getByLabel('Receipt total').fill((total / 100).toFixed(2))
  // A 1x1 PNG stands in for the receipt photo.
  const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==', 'base64')
  await maya.page.locator('input[type=file]').setInputFiles({ name: 'receipt.png', mimeType: 'image/png', buffer: png })
  const up = waitApi(maya, 'POST', /\/receipt-upload$/).catch(() => null)
  const r = waitApi(maya, 'POST', /\/purchased$/, 40_000)
  await maya.page.getByRole('button', { name: /^Paid/ }).click()
  const [u, res] = await Promise.all([up, r])
  if (!res.ok()) throw new Error(`purchased ${res.status()} ${await res.text()}`)
  const sent = res.request().postDataJSON() ?? {}
  return `receipt-upload ${u?.status() ?? 'not called'}; purchased sent receipt_path ${sent.receipt_path ?? 'none'}, receipt_total_cents ${sent.receipt_total_cents}`
})

await step('Maya picks up', '/run/run_demo', async () => {
  const r = waitApi(maya, 'POST', /\/picked-up$/)
  await maya.page.getByRole('button', { name: 'Picked up' }).click()
  const res = await r
  if (!res.ok()) throw new Error(`picked-up ${res.status()} ${await res.text()}`)
})

await step('the walk on the judge’s map, Got it, CO2 line', '/o/:id', async () => {
  await judge.page.getByText(/is bringing your food|is here/).first().waitFor({ timeout: 30_000 })
  await judge.page.screenshot({ path: out + 'walking.png' })
  await judge.page.getByText(/is here/).waitFor({ timeout: 150_000 })
  await judge.page.getByRole('button', { name: 'Got it' }).click()
  await judge.page.waitForTimeout(2000)
  await judge.page.screenshot({ path: out + 'got-it.png', fullPage: true })
  const txt = await judge.page.locator('body').innerText()
  const co2 = txt.match(/[^\n]*CO[2₂][^\n]*/)?.[0]
  if (!co2) throw new Error('no CO2 line on the order after Got it')
  return co2.trim()
})

await step('request a Ferry (POST /v1/requests)', 'API from the judge’s page', async () => {
  const runs = await fetchAs(judge, 'GET', `/v1/runs/run_demo`)
  const restaurant_id = runs.body?.restaurant?.id ?? runs.body?.restaurant_id
  const menu = await fetchAs(judge, 'GET', `/v1/restaurants/${restaurant_id}/menu`)
  const items = (menu.body?.categories ?? []).flatMap((c) => c.items)
  const item = items.find((i) => !i.options?.length) ?? items[0]
  const r = await fetchAs(judge, 'POST', '/v1/requests', {
    restaurant_id,
    lines: [{ menu_item_id: item?.id, qty: 1, choice_ids: [] }],
    tip_cents: 100,
    dropoff: order?.dropoff ?? { label: 'Davis Library', location: { lat: 35.9107, lng: -79.0479 } },
    needed_by: new Date(Date.now() + 45 * 60e3).toISOString(),
  })
  const problems = check.response('POST', '/v1/requests', r.status, r.body)
  if (r.status >= 300 || problems.length) throw new Error(`POST /v1/requests ${r.status} ${JSON.stringify(r.body).slice(0, 200)} ${problems.join('; ')}`)
  return `${r.status} ${r.body?.request?.id ?? r.body?.id ?? ''}`
})

await step('meet-again with a place in cir_climb, and Leo answers', 'API (Maya) → /i/:id (Leo)', async () => {
  const circles = await fetchAs(maya, 'GET', '/v1/circles')
  const circle = (circles.body ?? []).find((c) => c.id === 'cir_climb')
  if (!circle) throw new Error('Maya is not in cir_climb')
  const places = await fetchAs(maya, 'GET', '/v1/places')
  const place = places.body?.[0]
  const r = await fetchAs(maya, 'POST', `/v1/circles/${circle.id}/meet-again`, {
    message: 'Again Thursday?',
    proposed_time: new Date(Date.now() + 26 * 3600e3).toISOString(),
    place: place ? { id: place.id, label: place.label, location: place.location } : undefined,
  })
  if (r.status >= 300) throw new Error(`meet-again ${r.status} ${JSON.stringify(r.body).slice(0, 200)}`)
  const inv = r.body?.invite ?? r.body
  const p1 = check.response('POST', `/v1/circles/${circle.id}/meet-again`, r.status, r.body)
  if (p1.length) throw new Error(p1.join('; '))
  await leo.page.goto(`${base}/i/${inv.id}`)
  await leo.page.waitForTimeout(1500)
  await leo.page.screenshot({ path: out + 'invite.png', fullPage: true })
  const answered = waitApi(leo, 'POST', /\/respond$/)
  await leo.page.getByRole('button', { name: 'I’m in' }).click()
  const a = await answered
  if (!a.ok()) throw new Error(`respond ${a.status()} ${await a.text()}`)
  return `invite ${inv.id} place ${JSON.stringify(inv.place?.label ?? null)}; answered ${a.status()}`
})

for (const [path, name] of [
  ['/wallet', 'wallet'],
  ['/impact', 'impact'],
  ['/home', 'home and notifications'],
]) {
  await step(name, path, async () => {
    await judge.page.goto(base + path)
    await judge.page.waitForTimeout(2500)
    await judge.page.screenshot({ path: out + name.split(' ')[0] + '.png', fullPage: true })
    const txt = await judge.page.locator('body').innerText()
    if (/Something went wrong|Can’t reach Ferry/i.test(txt)) throw new Error(`error state on ${path}`)
  })
}

await step('the stream', 'WebSocket /v1/stream', async () => {
  const types = [...new Set(events.map((e) => `${e.who}:${e.type}`))]
  const bad = events.filter((e) => e.problems.length)
  if (!events.length) throw new Error('no stream events received')
  if (bad.length) throw new Error(`${bad.length} events off-contract: ${bad.slice(0, 3).map((e) => `${e.type} ${e.problems.join('; ')}`).join(' | ')}`)
  return `${events.length} events: ${types.join(', ')}`
})

const offContract = traffic.filter((t) => t.problems?.length)
writeFileSync(out + 'results.json', JSON.stringify({ base, api: API, steps, quote, order, offContract, traffic, events }, null, 2))
console.log('\n| step | screen | API calls | result |\n|---|---|---|---|')
for (const s of steps) console.log(`| ${s.step} | ${s.screen} | ${[...new Set(s.calls)].join('<br>') || '—'} | ${s.result}${s.note ? ': ' + s.note.replace(/\|/g, '/') : ''} |`)
console.log(`\n${traffic.filter((t) => t.method).length} REST responses, ${offContract.length} off-contract; ${events.length} stream events`)
await browser.close()
process.exit(steps.some((s) => s.result === 'red') ? 1 : 0)
