import { expect, test } from '@playwright/test'
import { checkA11y, freshMock, noHorizontalScroll, shot, signInAs, watchConsole } from './helpers'

// A tiny JPEG-ish PNG is enough: the app shrinks and re-encodes it before upload.
const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAFklEQVR42mP8z8Dwn4EIwMgwqpBBAAQ0Af8Q8fJXAAAAAElFTkSuQmCC', 'base64')

test('Maya runs the ferry: close, paid with a receipt photo, picked up, delivered by code', async ({ page }, info) => {
  const problems = watchConsole(page)
  await freshMock(page)
  await signInAs(page, 'maya')
  await expect(page.getByText(/You’re bringing food from Cosmic Cantina/)).toBeVisible()
  await page.getByRole('link', { name: 'Captain mode for Cosmic Cantina' }).click()
  await page.waitForURL(/\/run\/run_demo/)
  await expect(page.getByText('Shopping list')).toBeVisible()
  await expect(page.getByText(/For Dana/)).toBeVisible()
  await noHorizontalScroll(page)
  await checkA11y(page, 'runner open')
  await shot(page, info, 'runner-open')

  await page.getByRole('button', { name: 'Close window' }).click()
  await expect(page.getByRole('heading', { name: 'Paid at the counter?' })).toBeVisible()
  await page.getByLabel('Receipt total').fill('10.21')
  await page.locator('input[type=file]').setInputFiles({ name: 'receipt.png', mimeType: 'image/png', buffer: png })
  await expect(page.getByRole('button', { name: 'Retake photo' })).toBeVisible()
  await checkA11y(page, 'runner paid')
  await shot(page, info, 'runner-paid')
  await page.getByRole('button', { name: 'Paid' }).click()
  await expect(page.getByRole('heading', { name: 'Got the bags?' })).toBeVisible()
  await page.getByRole('button', { name: 'Picked up' }).click()
  await expect(page.getByRole('heading', { name: 'On the way' })).toBeVisible()
  await expect(page.getByText('Drop-offs')).toBeVisible()

  // Dana reads her code from her screen.
  const code = await page.evaluate(() => JSON.parse(localStorage.getItem('ferry.mock.v6')!).codes) as Record<string, string>
  const danaOrder = Object.keys(code).find((k) => k !== 'ord_demo')!
  await page.getByLabel('Dana’s code').fill(code[danaOrder])
  await page.getByRole('button', { name: 'Confirm' }).click()
  await expect(page.getByRole('heading', { name: 'All delivered' }).first()).toBeVisible()
  await shot(page, info, 'runner-done')
  expect(problems).toEqual([])
})
