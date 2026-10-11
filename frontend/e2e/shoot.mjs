// Quick screenshot helper: node e2e/shoot.mjs <path> [name] [persona]
import { chromium } from '@playwright/test'
const base = process.env.BASE ?? 'http://localhost:5173'
const [path = '/', name = 'shot', persona] = process.argv.slice(2)
const out = process.env.OUT ?? 'e2e/screens'
const sizes = (process.env.SIZES ?? '390x844,844x390,768x1024,1440x900').split(',')
const browser = await chromium.launch()
for (const sz of sizes) {
  const [w, h] = sz.split('x').map(Number)
  const ctx = await browser.newContext({ viewport: { width: w, height: h }, deviceScaleFactor: 1, reducedMotion: process.env.RM ? 'reduce' : 'no-preference' })
  const page = await ctx.newPage()
  const errors = []
  page.on('console', (m) => (m.type() === 'error' || m.type() === 'warning') && errors.push(`${m.type()}: ${m.text()}`))
  page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`))
  if (persona) {
    await page.goto(base + '/')
    await page.evaluate((p) => localStorage.setItem('ferry.token', `mock.usr_${p}`), persona)
  }
  if (process.env.SETUP === 'order') {
    // Place Jordan's demo order through the mocked API, then let the demo clock run.
    await page.goto(base + '/home')
    await page.waitForTimeout(1200)
    await page.evaluate(async () => {
      await fetch('https://api.ferry.mock/v1/runs/run_demo/orders', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Authorization: 'Bearer mock.usr_judge' },
        body: JSON.stringify({ lines: [{ menu_item_id: 'itm_cosmic_bean_cheese_burrito', qty: 1, choice_ids: ['cho_cosmic_bean_cheese_burrito_black_beans'] }], tip_cents: 200, dropoff: { id: 'place_fetzer', label: 'Fetzer Gym (HackNC)', location: { lat: 35.9093, lng: -79.0468 } } }),
      })
    })
  }
  await page.goto(base + path)
  await page.waitForTimeout(Number(process.env.WAIT ?? 1800))
  if (process.env.PAGES || process.env.FULL) {
    // Scroll through once so scroll reveals fire, then back to the top.
    const H = await page.evaluate(() => document.documentElement.scrollHeight)
    for (let y = 0; y < H; y += Math.round(h * 0.6)) {
      await page.evaluate((yy) => window.scrollTo(0, yy), y)
      await page.waitForTimeout(250)
    }
    await page.waitForTimeout(1200)
    await page.evaluate(() => window.scrollTo(0, 0))
  }
  if (process.env.PAGES) {
    const total = await page.evaluate(() => document.documentElement.scrollHeight)
    const n = Math.min(Number(process.env.PAGES), Math.ceil(total / h))
    for (let i = 0; i < n; i++) {
      await page.screenshot({ path: `${out}/${name}-${sz}-p${i}.png`, fullPage: true, clip: { x: 0, y: i * h, width: w, height: Math.min(h, total - i * h) } })
    }
  } else await page.screenshot({ path: `${out}/${name}-${sz}.png`, fullPage: !!process.env.FULL })
  if (errors.length) console.log(sz, errors.join('\n'))
  await ctx.close()
}
await browser.close()
console.log('ok')
