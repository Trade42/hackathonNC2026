import { expect, test } from '@playwright/test'
import { checkA11y, freshMock, noHorizontalScroll, shot, signInAs, watchConsole } from './helpers'

test('landing: the top bar stays above the page and a person brings the food', async ({ page }) => {
  const problems = watchConsole(page)
  await page.goto('/')
  await expect(page.getByRole('heading', { name: 'Who brings your food?' })).toBeAttached()
  await expect(page.getByText('A student heading that way does.', { exact: false })).toBeAttached()
  const nav = page.getByRole('navigation', { name: 'Ferry' })
  for (const y of [700, 1600, 2600]) {
    await page.evaluate((yy) => window.scrollTo(0, yy), y)
    await page.waitForTimeout(500)
    const box = (await nav.boundingBox())!
    // Whatever is under the bar's centre must be part of the bar.
    const onTop = await page.evaluate(
      ([x, yy]) => !!document.elementFromPoint(x, yy)?.closest('nav[aria-label="Ferry"]'),
      [box.x + box.width / 2, box.y + box.height / 2],
    )
    expect(onTop, `nav covered at scroll ${y}`).toBe(true)
  }
  await expect(nav).toHaveAttribute('data-scrolled', 'true')
  const blur = await nav.evaluate((el) => getComputedStyle(el).backdropFilter)
  expect(blur).toContain('blur(')
  expect(problems).toEqual([])
})

test('home shows the two main actions, live ferries, and cuisine search', async ({ page }, info) => {
  const problems = watchConsole(page)
  await freshMock(page)
  await signInAs(page, 'judge')
  await expect(page.getByRole('link', { name: /Request a ferry A student/ })).toBeVisible()
  await expect(page.getByRole('link', { name: /I’m going somewhere Bring food/ })).toBeVisible()
  await expect(page.getByRole('region', { name: 'Coming to you' })).toBeVisible()
  await expect(page.getByText('Ferry, your order assistant')).toBeVisible()
  await expect(page.getByRole('list', { name: 'Ferries leaving soon' })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Browse food' })).toBeVisible()
  await noHorizontalScroll(page)
  await checkA11y(page, 'home feed')
  await shot(page, info, 'home-feed')

  await page.getByRole('link', { name: 'Mexican', exact: true }).click()
  await page.waitForURL(/\/search\?cuisine=taco/)
  await expect(page.getByText('1 place')).toBeVisible()
  await expect(page.getByRole('link', { name: /Cosmic Cantina/ })).toBeVisible()
  await page.getByRole('radio', { name: 'All' }).click()
  await expect(page.getByText(/^[2-9] places$/)).toBeVisible()
  await checkA11y(page, 'search')
  expect(problems).toEqual([])
})

test('You shows the wallet and CO2; an order on its way floats above the tabs', async ({ page }, info) => {
  const problems = watchConsole(page)
  await freshMock(page)
  await signInAs(page, 'judge', '/you')
  await expect(page.getByText('Ferry Wallet')).toBeVisible()
  await expect(page.getByText('CO2 you saved')).toBeVisible()
  await checkA11y(page, 'you')

  await page.evaluate(async () => {
    await fetch('https://api.ferry.mock/v1/runs/run_demo/orders', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: 'Bearer mock.usr_judge' },
      body: JSON.stringify({ lines: [{ menu_item_id: 'itm_cosmic_bean_cheese_burrito', qty: 1, choice_ids: ['cho_cosmic_bean_cheese_burrito_black_beans'] }], tip_cents: 200, dropoff: { id: 'place_fetzer', label: 'Fetzer Gym (HackNC)', location: { lat: 35.9093, lng: -79.0468 } } }),
    })
  })
  await page.getByRole('link', { name: 'Search', exact: true }).first().click()
  const mini = page.getByRole('link', { name: /Maya will pick up your order.*Track your order/ })
  await expect(mini).toBeVisible()
  await expect(mini).toHaveAttribute('href', /\/o\//)
  await shot(page, info, 'mini-tracker')
  expect(problems).toEqual([])
})
