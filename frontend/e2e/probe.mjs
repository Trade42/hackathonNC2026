// Prints computed layout facts for an element: node e2e/probe.mjs <path> <selector> [persona]
import { chromium } from '@playwright/test'
const [path, sel, persona] = process.argv.slice(2)
const browser = await chromium.launch()
const page = await (await browser.newContext({ viewport: { width: 390, height: 844 } })).newPage()
if (persona) {
  await page.goto('http://localhost:5173/')
  await page.evaluate((p) => localStorage.setItem('ferry.token', `mock.usr_${p}`), persona)
}
await page.goto('http://localhost:5173' + path)
await page.waitForTimeout(2000)
console.log(
  await page.evaluate((s) => {
    const el = document.querySelector(s)
    if (!el) return 'not found'
    const chain = []
    for (let e = el; e && e !== document.body; e = e.parentElement) {
      const cs = getComputedStyle(e)
      chain.push(`${e.tagName}.${e.className} w=${e.getBoundingClientRect().width} display=${cs.display} pos=${cs.position} jc=${cs.justifyItems}/${cs.justifyContent}`)
    }
    return chain.join('\n')
  }, sel),
)
await browser.close()
