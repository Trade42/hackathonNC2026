// Renders the social image, web icons, and the native app icon and splash:
// node e2e/assets/render.mjs  (then `npx capacitor-assets generate`)
import { chromium } from '@playwright/test'
import { mkdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
const here = (f) => 'file://' + fileURLToPath(new URL(f, import.meta.url))
mkdirSync('resources', { recursive: true })
const b = await chromium.launch()
const p = await b.newPage({ viewport: { width: 1200, height: 630 } })
await p.goto(here('./og.html'))
await p.waitForTimeout(400)
await p.screenshot({ path: 'public/og.png' })
for (const [size, name] of [
  [512, 'public/icon-512.png'],
  [180, 'public/apple-touch-icon.png'],
  [192, 'public/icon-192.png'],
  [1024, 'resources/icon.png'],
]) {
  const q = await b.newPage({ viewport: { width: 512, height: 512 }, deviceScaleFactor: size / 512 })
  await q.goto(here('./icon.html'))
  await q.screenshot({ path: name })
}
const sp = await b.newPage({ viewport: { width: 1366, height: 1366 }, deviceScaleFactor: 2 })
await sp.goto(here('./splash.html'))
await sp.waitForTimeout(400)
await sp.screenshot({ path: 'resources/splash.png' })
await b.close()
console.log('rendered')
