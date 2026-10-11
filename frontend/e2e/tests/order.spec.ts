import { expect, test } from '@playwright/test'
import { checkA11y, freshMock, mock, noHorizontalScroll, shot, signInAs, watchConsole } from './helpers'

test('judge joins Maya’s ferry from the group-chat link and gets the food', async ({ page }, info) => {
  const problems = watchConsole(page)
  await freshMock(page)

  // Deep link from iMessage, signed out: sign in first, then land on the run.
  await page.goto('/r/run_demo')
  await page.waitForURL(/\/signin/)
  await page.getByRole('button', { name: /Continue as Jordan/ }).click()
  await page.waitForURL(/\/r\/run_demo$/)
  await expect(page.getByRole('heading', { name: /Maya is going to Cosmic Cantina/ })).toBeVisible()
  await noHorizontalScroll(page)
  await checkA11y(page, 'join run')
  await shot(page, info, 'join-run')

  await page.getByRole('button', { name: /Chicken Burrito/ }).click()
  await expect(page.getByRole('dialog')).toBeVisible()
  await page.getByLabel('Guacamole').check()
  await checkA11y(page, 'item sheet')
  await shot(page, info, 'item-sheet')
  await page.getByRole('button', { name: /Add to order/ }).click()
  const quoted = page.waitForResponse((response) => response.url().endsWith('/v1/runs/run_demo/quote') && response.request().method() === 'POST')
  await page.locator('#cart-bar').click()
  await page.waitForURL(/checkout/)
  const price = await (await quoted).json() as { ferry_fee_cents: number; total_cents: number }
  expect(price.ferry_fee_cents).toBe(299)
  await expect(page.getByText('Ferry fee', { exact: true }).locator('..')).toContainText(`$${(price.ferry_fee_cents / 100).toFixed(2)}`)
  await expect(page.getByText('Total before handoff', { exact: true }).locator('..')).toContainText(`$${(price.total_cents / 100).toFixed(2)}`)
  await expect(page.getByText('Sandbox money (Capital One Nessie)')).toBeVisible()
  await noHorizontalScroll(page)
  await checkA11y(page, 'checkout')
  await shot(page, info, 'checkout')
  await page.getByRole('button', { name: /^Place order/ }).click()

  await page.waitForURL(/\/o\/ord_demo/)
  await expect(page.getByRole('heading', { name: 'Order confirmed' })).toBeVisible()
  await expect(page.getByText(/\$\d+\.\d\d/).first()).toBeVisible()
  await checkA11y(page, 'tracking funded')
  await shot(page, info, 'track-funded')

  await mock(page, 'skipStep') // window closes
  await mock(page, 'skipStep') // bought
  await expect(page.getByRole('heading', { name: /Maya bought your food/ })).toBeVisible()
  await expect(page.getByRole('button', { name: /receipt photo/ })).toBeVisible()
  await mock(page, 'skipStep') // picked up
  await expect(page.getByRole('heading', { name: /Maya is bringing your food/ })).toBeVisible()
  await expect(page.locator('#map-chip')).toContainText(/min|Here/)
  await page.waitForTimeout(2500)
  await shot(page, info, 'track-walking')
  await mock(page, 'skipStep') // arrives
  await expect(page.getByRole('heading', { name: /Maya is here/ })).toBeVisible()
  await checkA11y(page, 'tracking arrived')

  await page.getByRole('button', { name: 'Got it' }).click()
  await expect(page.getByRole('dialog', { name: /CO2 saved/ })).toBeVisible()
  await expect(page.getByText('vs a car delivery (EPA estimate)')).toBeVisible()
  await page.waitForTimeout(700)
  await shot(page, info, 'celebration')
  await page.getByRole('button', { name: 'Enjoy your food' }).click()
  await expect(page.getByRole('heading', { name: 'Delivered' })).toBeVisible()
  expect(problems).toEqual([])
})

test('a closed window is explained and keeps the cart', async ({ page }) => {
  await freshMock(page)
  await signInAs(page, 'judge')
  await page.goto('/r/run_demo')
  await page.getByRole('button', { name: /Chips & Salsa/ }).click()
  await page.getByRole('button', { name: /Add to order/ }).click()
  await page.locator('#cart-bar').click()
  await page.waitForURL(/checkout/)
  // Maya closes the window from her phone before Jordan taps Place order.
  await page.evaluate(async () => {
    await fetch('https://api.ferry.mock/v1/runs/run_demo/close', { method: 'POST', headers: { Authorization: 'Bearer mock.usr_maya' } })
  })
  await expect(page.getByText(/already closed the window/)).toBeVisible()
  await expect(page.getByRole('button', { name: /^Place order/ })).toBeDisabled()
  await expect(page.getByRole('link', { name: 'Request a ferry' })).toBeVisible()
})
