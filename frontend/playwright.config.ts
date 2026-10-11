import { defineConfig } from '@playwright/test'

// Every flow runs at the four sizes from the brief.
const sizes = {
  'phone-portrait': { width: 390, height: 844 },
  'phone-landscape': { width: 844, height: 390 },
  tablet: { width: 768, height: 1024 },
  desktop: { width: 1440, height: 900 },
}

export default defineConfig({
  testDir: 'e2e/tests',
  timeout: 90_000,
  expect: { timeout: 15_000 },
  fullyParallel: true,
  workers: process.env.CI ? 2 : 4,
  reporter: [['list']],
  use: {
    baseURL: process.env.BASE ?? 'http://localhost:5173',
    trace: 'retain-on-failure',
  },
  projects: Object.entries(sizes).map(([name, viewport]) => ({
    name,
    use: { viewport, hasTouch: name.startsWith('phone'), isMobile: false },
  })),
  webServer: process.env.BASE
    ? undefined
    : { command: 'npm run dev -- --port 5173 --strictPort', url: 'http://localhost:5173', reuseExistingServer: true, timeout: 60_000 },
})
