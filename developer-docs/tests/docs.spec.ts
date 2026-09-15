import {test, expect, type Page} from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';

// Starlight routes: latest stable at the /docs/ root (no version prefix),
// zh primary, en TranslationPending stubs under /docs/en/.

async function openDocsPage(page: Page, path: string) {
  return page.goto(`/docs${path}`, {waitUntil: 'networkidle'});
}

test.describe('routing', () => {
  test('serves the docs root with a heading', async ({page}) => {
    await openDocsPage(page, '/');
    await expect(page.locator('h1')).toContainText('Migo 开发者文档');
    await expect(page.locator('.portal-links a', {hasText: 'SDK 下载'})).toBeVisible();
  });

  test('routes to the Android guide', async ({page}) => {
    await openDocsPage(page, '/getting-started/android/');
    await expect(page.getByRole('heading', {name: /Android/i}).first()).toBeVisible();
  });

  test('routes to the SDK architecture guide', async ({page}) => {
    await openDocsPage(page, '/concepts/sdk-architecture/');
    await expect(page.getByRole('heading', {name: /SDK 架构/}).first()).toBeVisible();
  });

  test('routes to the C ABI reference', async ({page}) => {
    await openDocsPage(page, '/reference/overview/');
    await expect(page.getByRole('heading', {name: /C ABI|API 参考/}).first()).toBeVisible();
  });

  test('routes to the download verification guide', async ({page}) => {
    await openDocsPage(page, '/release/download-verification/');
    await expect(page.getByRole('heading', {name: /下载与校验/}).first()).toBeVisible();
  });

  test('has no Next version', async ({page}) => {
    const response = await openDocsPage(page, '/next/');
    expect(response?.status()).toBe(404);
  });

  test('marks English stub pages as noindex and links back to zh', async ({page}) => {
    await openDocsPage(page, '/en/');
    await expect(page.locator('meta[name="robots"]')).toHaveAttribute('content', /noindex/i);
    await expect(page.locator('.sl-markdown-content')).toContainText('Translation pending');
    await expect(page.locator('a', {hasText: '中文版本'})).toHaveAttribute('href', '/docs/');
  });
});

test('no-JS readability', async ({browser}) => {
  const context = await browser.newContext({javaScriptEnabled: false});
  const page = await context.newPage();
  try {
    await openDocsPage(page, '/concepts/sdk-architecture/');
    await expect(page.locator('h1')).toBeVisible();
    // Mermaid renders client-side; prose explanations carry the content.
    await expect(page.locator('body')).toContainText('文字说明');
  } finally {
    await context.close();
  }
});

test('zh search indexes an API term', async ({page}) => {
  await openDocsPage(page, '/');
  await page.locator('button[data-open-modal]').first().click();
  const input = page.locator('.pagefind-ui__search-input');
  await input.fill('session');
  await expect(page.locator('.pagefind-ui__result').first()).toBeVisible({timeout: 10_000});
});

test('mobile layout fits the viewport', async ({page, isMobile}) => {
  test.skip(!isMobile, 'mobile project only');
  await openDocsPage(page, '/');
  const fitsViewport = await page.evaluate(
    () => document.body.scrollWidth <= document.body.clientWidth,
  );
  expect(fitsViewport).toBe(true);
});

test.describe('axe accessibility', () => {
  for (const path of ['/', '/reference/overview/']) {
    test(`has no serious or critical violations on ${path}`, async ({page}) => {
      await openDocsPage(page, path);
      const results = await new AxeBuilder({page}).analyze();
      const seriousViolations = results.violations.filter(
        ({impact}) => impact === 'serious' || impact === 'critical',
      );
      expect(seriousViolations).toEqual([]);
    });
  }
});
