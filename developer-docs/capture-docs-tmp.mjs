// 全站全页截图:served dev 8097,4 个视口段,存储 /data/work/tmp/review/
import { chromium } from 'playwright';
const pages = ['/', '/getting-started/android/', '/concepts/sdk-architecture/',
  '/reference/overview/', '/reference/session/', '/reference/surface/', '/reference/engine/',
  '/reference/input/', '/reference/external-frames/', '/reference/capabilities/', '/reference/types/',
  '/release/download-verification/',
  // en stub 2 样本
  '/en/', '/en/getting-started/android/'];
const viewports = [
  { name: 'desktop', width: 1920, height: 1080, dpr: 1.25 },
  { name: 'mobile',  width: 390,  height: 844,  dpr: 3 },
];
const b = await chromium.launch();
for (const p of pages) {
  for (const v of viewports) {
    const pg = await b.newPage({ viewport: { width: v.width, height: v.height }, deviceScaleFactor: v.dpr });
    const err = [];
    pg.on('console', m => { if (m.type() === 'error') err.push(m.text().slice(0, 120)); });
    pg.on('pageerror', e => err.push('PE: ' + String(e).slice(0, 120)));
    const req = [];
    pg.on('requestfailed', r => req.push(r.url().split('/').pop() + ' ' + r.failure()?.errorText));
    try {
      await pg.goto('http://127.0.0.1:8097/docs' + (p === '/' ? '/' : p), { waitUntil: 'networkidle', timeout: 30000 });
      await pg.waitForTimeout(1500);
      const slug = (p === '/' ? 'index' : p).replace(/\//g, '_');
      await pg.screenshot({ path: `/data/work/tmp/review/${slug}-${v.name}-full.png`, fullPage: true });
      const title = await pg.title();
      console.log(`OK ${p} (${v.name}) "${title}" fullH=${await pg.evaluate(() => document.body.scrollHeight)} errs=[${err.join(' | ')}] req-fail=[${req.join(' | ')}]`);
    } catch (e) { console.log('FAIL', p, v.name, String(e).slice(0, 120)); }
    await pg.close();
  }
}
await b.close();
