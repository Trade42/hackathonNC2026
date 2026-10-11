import AxeBuilder from '@axe-core/playwright'
import { expect, type Page, type TestInfo } from '@playwright/test'

export const live = process.env.LIVE === '1'

// Console errors and warnings fail the test (except headless GPU chatter).
export function watchConsole(page: Page) {
  const problems: string[] = []
  page.on('console', (m) => {
    if (m.type() !== 'error' && m.type() !== 'warning') return
    const t = m.text()
    if (/GL Driver|GPU stall|WebGL|Failed to load resource.*(tiles|openfreemap)/i.test(t)) return
    problems.push(`${m.type()}: ${t}`)
  })
  page.on('pageerror', (e) => problems.push(`pageerror: ${e.message}`))
  return problems
}

export async function freshMock(page: Page) {
  await page.goto('/')
  // On first load MSW may claim the page and reload it. Wait for that to
  // finish before the test navigates to sign-in or measures the layout.
  if (!live) await page.waitForFunction(() => !!(window as unknown as { __ferryMock?: unknown }).__ferryMock)
  await page.evaluate(() => {
    localStorage.clear()
    sessionStorage.clear()
  })
}

export async function signInAs(page: Page, persona: 'judge' | 'maya' | 'dana' | 'leo', next = '/home') {
  await page.goto(`/signin?next=${encodeURIComponent(next)}`)
  await page.getByRole('button', { name: new RegExp(`Continue as ${persona === 'judge' ? 'Jordan' : persona[0].toUpperCase() + persona.slice(1)}`) }).click()
  await page.waitForURL((u) => u.pathname === next.split('?')[0])
}

export async function mock(page: Page, fn: 'skipStep' | 'reset' | 'triggerInvite') {
  await page.evaluate((f) => (window as unknown as { __ferryMock: Record<string, () => void> }).__ferryMock[f](), fn)
}

export async function checkA11y(page: Page, label: string) {
  await page.waitForTimeout(1000) // let entrance animations finish so contrast is measured at rest
  const res = await new AxeBuilder({ page }).exclude('.maplibregl-canvas').exclude('.maplibregl-ctrl-attrib').exclude('[data-pop-layer]').analyze()
  const bad = res.violations.filter((v) => v.impact === 'serious' || v.impact === 'critical')
  expect(bad.map((v) => `${label}: ${v.id} (${v.impact}) ${v.nodes.map((n) => n.target.join(' ')).slice(0, 3).join(', ')}`)).toEqual([])
}

export async function noHorizontalScroll(page: Page) {
  const over = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)
  expect(over, 'page scrolls sideways').toBeLessThanOrEqual(1)
}

export async function shot(page: Page, info: TestInfo, name: string) {
  await page.screenshot({ path: `e2e/screens/${info.project.name}/${name}.png` })
}
