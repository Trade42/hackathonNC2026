import { expect, test } from '@playwright/test'
import { checkA11y, freshMock, noHorizontalScroll, signInAs, watchConsole } from './helpers'

test('iPhone install guide opens from the landing and has the app metadata', async ({ page, request }) => {
  const problems = watchConsole(page)
  await page.goto('/#get')
  await page.getByRole('link', { name: 'Set up on iPhone' }).click()
  await expect(page).toHaveURL(/\/install\/ios$/)
  await expect(page.getByRole('heading', { name: 'Ferry, one tap away.' })).toBeVisible()
  await expect(page.getByRole('listitem')).toHaveCount(5)
  await expect(page.getByRole('heading', { name: 'Choose Add to Home Screen' })).toBeVisible()
  await expect(page.getByText('Open as Web App', { exact: true })).toBeAttached()
  await expect(page.getByText(/Home Screen apps keep their own sign-in/)).toBeAttached()
  await noHorizontalScroll(page)
  await checkA11y(page, 'iPhone install guide')

  const manifest = await (await request.get('/manifest.webmanifest')).json()
  expect(manifest).toMatchObject({ id: '/', start_url: '/home', scope: '/', display: 'standalone' })
  expect(manifest.icons).toEqual(expect.arrayContaining([expect.objectContaining({ sizes: '180x180' })]))
  expect(await page.locator('meta[name="apple-mobile-web-app-capable"]').getAttribute('content')).toBe('yes')
  expect(problems).toEqual([])
})

test('an installed-style app launch keeps the signed-in session', async ({ page }) => {
  const problems = watchConsole(page)
  await freshMock(page)
  await signInAs(page, 'judge')
  await expect(page.getByRole('heading', { name: /Hungry, Jordan/ })).toBeVisible()
  const token = await page.evaluate(() => localStorage.getItem('ferry.token'))
  expect(token).toBeTruthy()

  await page.reload()
  await expect(page).toHaveURL(/\/home$/)
  await expect(page.getByRole('heading', { name: /Hungry, Jordan/ })).toBeVisible()
  expect(await page.evaluate(() => localStorage.getItem('ferry.token'))).toBe(token)
  expect(problems).toEqual([])
})

test('a returning demo session keeps open trip return plans in the future', async ({ page }) => {
  await freshMock(page)
  await signInAs(page, 'judge')
  await page.evaluate(() => {
    const key = 'ferry.mock.v6'
    const state = JSON.parse(localStorage.getItem(key)!)
    const old = new Date(Date.now() - 60 * 60_000).toISOString()
    for (const id of ['run_demo', 'run_insomnia1']) {
      state.runs[id].window_closes_at = old
      state.runs[id].leaving_at = old
      state.runs[id].returning_at = old
    }
    state.sim.tickOwner = ''
    state.sim.tickUntil = 0
    localStorage.setItem(key, JSON.stringify(state))
  })
  await page.reload()
  await page.waitForFunction(() => {
    const state = JSON.parse(localStorage.getItem('ferry.mock.v6')!)
    return Date.parse(state.runs.run_demo.window_closes_at) > Date.now() + 5 * 60_000
  })
  const plan = await page.evaluate(() => {
    const state = JSON.parse(localStorage.getItem('ferry.mock.v6')!)
    return [state.runs.run_demo, state.runs.run_insomnia1]
  })
  for (const run of plan) {
    expect(Date.parse(run.returning_at)).toBeGreaterThan(Date.parse(run.leaving_at))
  }
})

test('an already extended demo trip repairs a stale return time', async ({ page }) => {
  await freshMock(page)
  await signInAs(page, 'judge')
  await page.evaluate(() => {
    const key = 'ferry.mock.v6'
    const state = JSON.parse(localStorage.getItem(key)!)
    const now = Date.now()
    state.runs.run_demo.leaving_at = new Date(now + 4 * 60_000).toISOString()
    state.runs.run_demo.window_closes_at = new Date(now + 10 * 60_000).toISOString()
    state.runs.run_demo.returning_at = new Date(now - 60 * 60_000).toISOString()
    state.sim.tickOwner = ''
    state.sim.tickUntil = 0
    localStorage.setItem(key, JSON.stringify(state))
  })
  await page.reload()
  await page.waitForFunction(() => {
    const run = JSON.parse(localStorage.getItem('ferry.mock.v6')!).runs.run_demo
    return Date.parse(run.returning_at) > Date.parse(run.leaving_at)
  }, undefined, { timeout: 4000 })
})
