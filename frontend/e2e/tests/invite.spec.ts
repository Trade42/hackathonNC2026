import { expect, test } from '@playwright/test'
import { checkA11y, freshMock, mock, noHorizontalScroll, shot, signInAs, watchConsole } from './helpers'

test('Dana’s meetup invite arrives, the judge accepts, and the Telegram group opens', async ({ page }, info) => {
  const problems = watchConsole(page)
  await freshMock(page)
  await signInAs(page, 'judge')
  await expect(page.getByRole('heading', { name: 'Hungry, Jordan?' })).toBeVisible()
  await page.waitForTimeout(800) // let the stream connect so the event arrives live
  await mock(page, 'triggerInvite')
  const banner = page.getByRole('button', { name: /Dana wants to meet again/ })
  await expect(banner).toBeVisible()
  await shot(page, info, 'invite-banner')
  await banner.click()
  await page.waitForURL(/\/i\/inv_demo/)
  await noHorizontalScroll(page)
  await checkA11y(page, 'invite')
  await page.getByRole('button', { name: 'I’m in' }).click()
  await expect(page.getByRole('heading', { name: 'Your group chat is ready' })).toBeVisible({ timeout: 10_000 })
  await expect(page.getByRole('link', { name: 'Open in Telegram' })).toHaveAttribute('href', /^https:\/\/t\.me\//)
  // Not a phone browser here, so the group link is also a QR code.
  await expect(page.getByRole('img', { name: /QR code that opens the group in Telegram/ })).toBeVisible()
  await shot(page, info, 'invite-ready')
  expect(problems).toEqual([])
})
