// Records a page load to a video for checking motion: node e2e/record.mjs <outDir> [path] [WxH] [ms]
import { chromium } from '@playwright/test'
const [out, path = '/', size = '1440x900', ms = '9000'] = process.argv.slice(2)
const [width, height] = size.split('x').map(Number)
const browser = await chromium.launch()
const ctx = await browser.newContext({ viewport: { width, height }, recordVideo: { dir: out, size: { width, height } } })
const page = await ctx.newPage()
await page.goto((process.env.BASE ?? 'http://localhost:5173') + path)
await page.waitForTimeout(Number(ms))
await ctx.close()
await browser.close()
