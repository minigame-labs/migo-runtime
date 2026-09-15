import {defineConfig, devices} from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  outputDir: './test-results',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  timeout: 30_000,
  use: {
    baseURL: 'http://127.0.0.1:8097',
    trace: 'on-first-retry',
  },
  projects: [
    {
      name: 'desktop',
      use: {
        ...devices['Desktop Chrome'],
        viewport: {width: 1280, height: 720},
        launchOptions: {
          executablePath: process.env.CHROME_PATH || undefined,
        },
      },
    },
    {
      name: 'mobile',
      use: {
        ...devices['Pixel 5'],
        viewport: {width: 390, height: 844},
        launchOptions: {
          executablePath: process.env.CHROME_PATH || undefined,
        },
      },
    },
  ],
  webServer: {
    command: 'npm run docs:serve',
    url: 'http://127.0.0.1:8097/docs/',
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
  },
  reporter: process.env.CI
    ? [['github'], ['html', {outputFolder: 'playwright-report', open: 'never'}]]
    : [['list']],
});
