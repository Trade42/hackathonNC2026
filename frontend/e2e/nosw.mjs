// Loads a page with no Service Worker API, like the iOS WebView, and prints
// console output: node e2e/nosw.mjs <path>
import { chromium } from '@playwright/test'
const [path = '/signin'] = process.argv.slice(2)
const browser = await chromium.launch()
const ctx = await browser.newContext({ viewport: { width: 390, height: 844 } })
await ctx.addInitScript(() => {
  delete Navigator.prototype.serviceWorker
})
const page = await ctx.newPage()
page.on('console', (m) => console.log('console', m.type(), m.text().slice(0, 200)))
page.on('pageerror', (e) => console.log('pageerror', e.message))
page.on('requestfailed', (r) => console.log('failed', r.method(), r.url()))
page.on('framenavigated', (f) => f === page.mainFrame() && console.log('nav', f.url()))
await page.goto((process.env.BASE ?? 'http://localhost:5173') + path)
await page.waitForTimeout(4000)
console.log('demo panel:', await page.locator('#demo-h').count(), 'sw in nav:', await page.evaluate(() => 'serviceWorker' in navigator))
await page.screenshot({ path: 'e2e/screens/nosw.png' })
await browser.close()
