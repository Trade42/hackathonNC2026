import { expect, test } from '@playwright/test'
import { checkA11y, noHorizontalScroll, shot, watchConsole } from './helpers'

test('landing shows the live campus counter, Telegram and the downloads', async ({ page }, info) => {
  const problems = watchConsole(page)
  await page.goto('/')
  await expect(page.getByRole('heading', { level: 1 })).toContainText('Someone’s already going.')
  await expect(page.getByText('one flat $2.99 Ferry fee', { exact: false })).toBeAttached()
  await expect(page.getByText('Captain base pay', { exact: true }).locator('..')).toContainText('$2.00')
  await expect(page.getByText('Ferry keeps before costs', { exact: true }).locator('..')).toContainText('$0.99')
  const phone = page.locator('header figure').first()
  const hero = page.locator('header').first()
  const phoneBox = await phone.boundingBox()
  const heroBox = await hero.boundingBox()
  expect(phoneBox && heroBox && phoneBox.y + phoneBox.height <= heroBox.y + heroBox.height, 'chat phone stays within the hero').toBe(true)
  await expect(page.getByText(/kg/).first()).toBeVisible()
  await expect(page.getByText(/kept out of the air on campus/)).toBeVisible()
  await expect(page.getByRole('link', { name: 'Message Ferry on Telegram' }).first()).toBeVisible()
  await expect(page.getByRole('link', { name: 'Add Ferry to your group chat' })).toHaveAttribute('href', /t\.me\/.+startgroup/)
  await expect(page.getByRole('link', { name: 'Set up on iPhone' })).toHaveAttribute('href', '/install/ios')
  await expect(page.getByRole('link', { name: 'Download for Android' })).toHaveAttribute('href', /\/downloads\/ferry\.apk$/)
  // The website is for the landing and downloads: no sign-in, no Mac app.
  await expect(page.getByRole('link', { name: 'Sign in' })).toHaveCount(0)
  await expect(page.getByText(/\bMac\b/)).toHaveCount(0)
  // Scroll through so every section reveals, then check it all.
  for (const id of ['#how', '#telegram', '#impact', '#get']) {
    await page.locator(id).scrollIntoViewIfNeeded()
    await page.waitForTimeout(350)
  }
  await page.evaluate(() => window.scrollTo(0, 0))
  await noHorizontalScroll(page)
  await checkA11y(page, 'landing')
  await shot(page, info, 'landing')
  expect(problems).toEqual([])
})
