import { expect, test } from '@playwright/test'
import { checkA11y, freshMock, noHorizontalScroll, shot, signInAs, watchConsole } from './helpers'

const judgeScreens: [string, string, RegExp | string][] = [
  ['home', '/home', /Hungry, Jordan/],
  ['search', '/search', /\d+ places/],
  ['you', '/you', 'Ferry Wallet'],
  ['area', '/food/area_franklin', 'Cosmic Cantina'],
  ['restaurant', '/food/area_franklin/rst_limebasil', 'Beef Pho'],
  ['orders', '/orders', /In progress|Past/],
  ['wallet', '/wallet', 'Ferry Wallet balance'],
  ['impact', '/impact', 'CO2 saved each week'],
  ['circles', '/circles', 'Hinton James 4th floor'],
  ['circle', '/circles/cir_hinton4', 'Meet again'],
  ['new-run', '/run/new', 'Where are you going?'],
  ['settings', '/settings', 'Sound and motion'],
  ['not-found', '/nope', 'This page drifted off'],
]

test('every judge screen renders cleanly', async ({ page }, info) => {
  const problems = watchConsole(page)
  await freshMock(page)
  await signInAs(page, 'judge')
  for (const [name, path, text] of judgeScreens) {
    await page.goto(path)
    await expect(page.getByText(text).first()).toBeVisible()
    await page.waitForTimeout(350)
    await noHorizontalScroll(page)
    await checkA11y(page, name)
    await shot(page, info, name)
  }
  expect(problems).toEqual([])
})

test('sign-in page offers Telegram and the demo personas', async ({ page }, info) => {
  await freshMock(page)
  await page.goto('/signin')
  await expect(page.getByRole('heading', { name: 'Sign in with Telegram' })).toBeVisible()
  await expect(page.getByText(/Telegram will show \d{4}\. Confirm only if it matches\./)).toBeVisible()
  await expect(page.getByRole('button', { name: /Continue as Jordan/ })).toBeVisible()
  await page.waitForTimeout(600)
  await noHorizontalScroll(page)
  await checkA11y(page, 'signin')
  await shot(page, info, 'signin')
})

test('nothing clips at 320 px wide', async ({ page }) => {
  test.skip(test.info().project.name !== 'phone-portrait', 'one width check is enough')
  await page.setViewportSize({ width: 320, height: 640 })
  await freshMock(page)
  await page.goto('/')
  await noHorizontalScroll(page)
  await signInAs(page, 'judge')
  for (const path of ['/home', '/r/run_demo', '/wallet', '/impact', '/circles', '/settings']) {
    await page.goto(path)
    await page.waitForTimeout(500)
    await noHorizontalScroll(page)
  }
})
