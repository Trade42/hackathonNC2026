// Measures a webfont against a local fallback to tune size-adjust and the
// vertical overrides: node e2e/fontmetrics.mjs ["Font Name" weight fallback]...
import { chromium } from '@playwright/test'
const pairs = [
  ['Instrument Serif', 400, 'Georgia'],
  ['PT Sans', 400, 'Arial'],
  ['Exo 2', 600, 'Arial'],
]
const b = await chromium.launch()
const p = await b.newPage()
await p.goto(process.env.BASE ?? 'http://localhost:5173/')
await p.waitForTimeout(1500)
for (const [font, weight, fallback] of pairs) {
  console.log(
    font,
    await p.evaluate(
      async ({ font, weight, fallback }) => {
        await document.fonts.load(`${weight} 100px "${font}"`)
        const c = document.createElement('canvas').getContext('2d')
        const text = 'Trips your friends already make become food runs. When a friend heads out for food, add your order.'
        c.font = `${weight} 100px "${font}"`
        const a = c.measureText(text)
        c.font = `${weight} 100px ${fallback}`
        const g = c.measureText(text)
        return { sizeAdjust: ((a.width / g.width) * 100).toFixed(1) + '%', ascent: a.fontBoundingBoxAscent + '%', descent: a.fontBoundingBoxDescent + '%' }
      },
      { font, weight, fallback },
    ),
  )
}
await b.close()
