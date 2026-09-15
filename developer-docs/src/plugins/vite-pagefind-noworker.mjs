/**
 * Pagefind worker mode resolves the index manifest against the SITE ROOT
 * instead of the `/docs/` base ("Failed to load Pagefind metadata"), so
 * search hangs. starlight's `pagefind` config schema `$strip`s unknown keys,
 * so `noWorker` cannot flow through `pagefindUserConfig` — even though
 * `Search.astro` spreads it into `PagefindUI` options.
 *
 * Instead of forking Search (and fighting with starlight-versions' overrides),
 * rewrite the tiny virtual module content right before the client bundle
 * consumes it. Component ownership stays 100% with starlight / the plugin;
 * this is a one-key JSON patch on a 5-token module.
 *
 * If upstream starlight exposes `noWorker` through its schema one day, delete
 * this file — nothing else references it.
 */
export default function pagefindNoWorker() {
  const TARGET_ID = 'virtual:starlight/pagefind-config';
  return {
    name: 'pagefind-no-worker',
    enforce: 'post',
    transform(code, id) {
      if (!id.includes(TARGET_ID)) return null;
      // code shape: `export const pagefindUserConfig = {…}` (possibly pretty)
      if (!code.includes('pagefindUserConfig')) return null;
      if (code.includes('noWorker')) return null; // already injected / upstream fixed
      if (!/export\s+const\s+pagefindUserConfig\s*=\s*/.test(code)) {
        // starlight 改了 virtual module 的形状 —— 注入目标消失,继续静默跑会
        // 换回"worker 在站点根找 meta"的老 bug(搜索挂死)。必须响亮失败。
        throw new Error(
          '[pagefind-no-worker] virtual:starlight/pagefind-config no longer matches the expected export shape. ' +
            'Check starlight release notes; if upstream now exposes noWorker through its pagefind schema, delete this plugin.',
        );
      }
      return {
        code: code.replace(
          /(export\s+const\s+pagefindUserConfig\s*=\s*)/,
          '$1/* injected by pagefind-no-worker */ Object.assign({ noWorker: true }, ',
        ).concat(')'),
        map: null,
      };
    },
  };
}
