import {test, expect, type Page} from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';

// Starlight routes: the current docs at the /docs/ root (no version prefix),
// zh primary, the English translation under /docs/en/, and the frozen 0.9
// archive under /docs/0.9/ (zh) and /docs/en/0.9/ (TranslationPending stubs).

async function openDocsPage(page: Page, path: string) {
  return page.goto(`/docs${path}`, {waitUntil: 'networkidle'});
}

test.describe('routing', () => {
  test('serves the docs root with a heading', async ({page}, testInfo) => {
    await openDocsPage(page, '/');
    await expect(page.locator('h1')).toContainText('Migo 开发者文档');
    const links = page.locator('.portal-links a');
    await expect(links.filter({hasText: '首页'})).toBeVisible();
    await expect(links.filter({hasText: '开发者文档'})).toBeVisible();
    // A phone's header keeps only the first two links: five do not fit at 390
    // px without colliding with the search icon (Header.astro, max-width 50rem).
    const download = links.filter({hasText: 'SDK 下载'});
    if (testInfo.project.name === 'mobile') {
      await expect(download).toBeHidden();
    } else {
      await expect(download).toBeVisible();
    }
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

  // The English 0.9 archive was a tree of Translation-pending stubs until the
  // 0.9.7 docs regenerated it as a full translation. This used to assert the
  // stub on /en/0.9/, which is a temporary state, and it went red the moment
  // the archive was completed. What holds now is that the archive is real
  // English and stays inside its own version: a link out of it lands an
  // archive reader in latest, or in Chinese.
  test('serves the English 0.9 archive translated, linking within the archive', async ({page}) => {
    await openDocsPage(page, '/en/0.9/');
    await expect(page.locator('.sl-markdown-content')).not.toContainText('Translation pending');
    const hrefs = await page.locator('.sl-markdown-content a[href^="/docs/"]').evaluateAll(
      (links) => links.map((link) => link.getAttribute('href')),
    );
    expect(hrefs.length).toBeGreaterThan(0);
    expect(hrefs.filter((href) => !href?.startsWith('/docs/en/0.9/'))).toEqual([]);
    // The one link that is meant to leave the archive is the version notice,
    // and an English reader is sent to English latest.
    await expect(page.locator('main a', {hasText: 'latest version'})).toHaveAttribute('href', '/docs/en/');
  });

  test('serves the English translation indexable, linking within English', async ({page}) => {
    await openDocsPage(page, '/en/');
    await expect(page.locator('meta[name="robots"][content*="noindex"]')).toHaveCount(0);
    await expect(page.locator('.sl-markdown-content')).not.toContainText('Translation pending');
    // A translated page that links into the zh tree drops an English reader
    // into Chinese on the first click.
    const hrefs = await page.locator('.sl-markdown-content a[href^="/docs/"]').evaluateAll(
      (links) => links.map((link) => link.getAttribute('href')),
    );
    expect(hrefs.length).toBeGreaterThan(0);
    expect(hrefs.filter((href) => !href?.startsWith('/docs/en/'))).toEqual([]);
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
