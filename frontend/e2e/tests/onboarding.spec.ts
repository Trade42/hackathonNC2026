import { expect, test } from '@playwright/test'
import { checkA11y, freshMock, noHorizontalScroll, shot, watchConsole } from './helpers'

test('a new user signs in with Telegram and finishes the questionnaire', async ({ page }, info) => {
  const problems = watchConsole(page)
  await freshMock(page)
  await page.goto('/signin')
  // Desktop also shows a QR of the bot link; tapping the button stands in for
  // tapping Start in the bot (mock mode).
  if (info.project.name === 'desktop') await expect(page.getByRole('img', { name: /QR code that opens the Ferry bot/ })).toBeVisible()
  await expect(page.getByText('Waiting for you to tap Start and Confirm in Telegram')).toBeVisible()
  await checkA11y(page, 'sign in')
  await page.getByRole('link', { name: /Continue with Telegram|Open Telegram here/ }).click()
  await page.waitForURL(/\/welcome/, { timeout: 20_000 })

  await expect(page.getByText('Step 1 of 8')).toBeVisible()
  await expect(page.getByText(/Ferry bot uses these answers/)).toBeVisible()
  await page.getByLabel('Your name').fill('Riley')
  await checkA11y(page, 'onboarding name')
  await shot(page, info, 'onboarding-1')
  await page.getByRole('button', { name: 'Next' }).click()

  await page.getByRole('radio', { name: 'Davis Library steps' }).click()
  await shot(page, info, 'onboarding-2')
  await page.getByRole('button', { name: 'Next' }).click()
  await page.getByRole('button', { name: 'Vegetarian' }).click()
  await page.getByRole('button', { name: 'Peanuts' }).click()
  await page.getByRole('button', { name: 'Next' }).click()
  await page.getByRole('button', { name: 'Vietnamese' }).click()
  await page.getByRole('button', { name: 'Lime & Basil' }).click()
  await page.getByRole('button', { name: 'Next' }).click()
  await page.getByRole('button', { name: 'Dinner' }).click()
  await page.getByRole('button', { name: 'Next' }).click()
  await page.getByRole('radio', { name: 'Brisk' }).click()
  await page.getByRole('button', { name: 'Next' }).click()
  await expect(page.getByRole('heading', { name: 'How far would you walk to drop food off?' })).toBeVisible()
  await page.getByRole('radio', { name: /800 m/ }).click()
  await page.getByRole('button', { name: 'Next' }).click()
  await expect(page.getByText('Step 8 of 8')).toBeVisible()
  await page.waitForTimeout(800) // the step slides in
  await noHorizontalScroll(page)
  await checkA11y(page, 'onboarding finish')
  await shot(page, info, 'onboarding-7')
  await page.getByRole('button', { name: 'Save and start' }).click()
  await page.waitForURL(/\/home/)
  await expect(page.getByRole('heading', { name: 'Hungry, Riley?' })).toBeVisible()
  expect(problems).toEqual([])
})
