import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import sitemap from '@astrojs/sitemap';
import mermaid from 'astro-mermaid';
import starlightLinksValidator from 'starlight-links-validator';
import starlightVersions from 'starlight-versions';
import pagefindNoWorker from './src/plugins/vite-pagefind-noworker.mjs';
import { docsConfig } from './docs.config.mjs';

export default defineConfig({
  site: 'https://minigame-labs.com',
  base: '/docs',
  trailingSlash: 'always',
  outDir: 'build',
  build: { format: 'directory' },
  vite: { plugins: [pagefindNoWorker()] },
  integrations: [
    mermaid(),
    sitemap({
      // 排除两类:0.9 冻结归档(zh 与 en) + Next 版。
      // 英文 latest 已于 2026-09-15 全量翻译完成并放行索引;
      // en/0.9 归档仍是 Translation-pending 占位,维持排除
      // (归档与 latest 内容重复,索引会互相争 canonical,spec §6.4)。
      filter: (page) => !page.includes('/docs/en/0.9/') && !page.includes('/docs/0.9/'),
    }),
    starlight({
      title: 'Migo 开发者文档',
      description: '面向原生小游戏运行时的集成、架构与 API 指南',
      logo: { src: './public/brand/mark.svg', alt: 'Migo' },
      favicon: '/img/favicon.svg',
      defaultLocale: 'root',
      locales: {
        root: { label: '简体中文', lang: 'zh-CN' },
        en: { label: 'English', lang: 'en' },
      },
      plugins: [
        // 2026-09-15 用户裁决:版本切换立即启用 — current=0.9.x 工作稿,
        // 归档 0.9 = 0.9.x 发布内容快照。0.10 发布时 current 变成 0.10 工作稿,
        // 插件自动把当时状态再落一份到归档,流程不变(README「发布新版本」)。
        starlightVersions({
          current: {
            // 跟随 release/VERSION 自动变(0.10 发布后自动叫 0.10.x)
            label: `${docsConfig.docsSeries}.x`,
          },
          // 归档 = 0.9.7 发布时的文档快照,内容冻结。后续 0.10 发布时在
          // 数组头部新增 { slug: '0.10', label: '0.10.0' },0.9 这行至历史尾部。
          versions: [{ slug: '0.9', label: '0.9.7' }],
        }),
        starlightLinksValidator(),
      ],
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/minigame-labs/migo-runtime' }],
      // Portal parity: every docs page advertises the same social card as the www site,
      // plus a machine-readable SDK version marker (docs 版本 = SDK 版本,单一事实源 release/VERSION)。
      head: [
        { tag: 'meta', attrs: { property: 'og:image', content: 'https://minigame-labs.com/brand/og.png' } },
        { tag: 'meta', attrs: { name: 'twitter:card', content: 'summary_large_image' } },
        { tag: 'meta', attrs: { name: 'docs-version', content: docsConfig.releaseVersion } },
      ],
      // autogenerate: hand-listed items silently drop new pages on the floor
      // (links-validator doesn't report orphan pages). Order within a group
      // comes from frontmatter `sidebar.order`.
      sidebar: [
        { label: '首页', translations: { en: 'Home' }, link: '/' },
        {
          label: '快速开始',
          translations: { en: 'Getting started' },
          collapsed: true,
          items: [{ autogenerate: { directory: 'getting-started' } }],
        },
        {
          label: '核心概念',
          translations: { en: 'Concepts' },
          collapsed: true,
          items: [{ autogenerate: { directory: 'concepts' } }],
        },
        {
          // 宿主能力独立成顶级组(IA spec §2/§3:按能力分页,runtime 级不变量不放平台组下)。
          // 11 页已全落地(handler 已随 0.9 release 出线,触发满足);单组 ≤9 页(§7)
          // 拆两个子组:商业化(ads/payment)+ 平台服务(馀 7 页),顶层只留 overview 与
          // 唯一的 C ABI 级能力 keyboard。层级 = 组 → 子组 → 页(§7 上限 3)。
          label: '宿主能力',
          translations: { en: 'Host capabilities' },
          collapsed: true,
          items: [
            { slug: 'capabilities/overview' },
            { slug: 'capabilities/keyboard' },
            {
              label: '商业化',
              translations: { en: 'Monetization' },
              collapsed: true,
              items: [{ autogenerate: { directory: 'capabilities/commerce' } }],
            },
            {
              label: '平台服务',
              translations: { en: 'Platform services' },
              collapsed: true,
              items: [{ autogenerate: { directory: 'capabilities/services' } }],
            },
          ],
        },
        {
          // 按 API 表面切子组(IA spec §4):C ABI 是表面一;平台绑定子组 trigger = 第二个
          // 平台快速开始落地(已满足,platform-binding/ 正在并入);Android SDK 子组 trigger =
          // 与宿主能力首批同批。新子组目录不放在 reference/ 下(autogenerate 递归吸子目录)。
          label: 'API 参考',
          translations: { en: 'API reference' },
          collapsed: true,
          items: [
            {
              label: 'C ABI',
              translations: { en: 'C ABI' },
              collapsed: true,
              items: [{ autogenerate: { directory: 'reference' } }],
            },
            {
              label: '平台 surface 绑定',
              translations: { en: 'Platform surface bindings' },
              collapsed: true,
              items: [{ autogenerate: { directory: 'platform-binding' } }],
            },
            {
              label: 'Android SDK',
              translations: { en: 'Android SDK' },
              collapsed: true,
              items: [{ autogenerate: { directory: 'android-sdk' } }],
            },
          ],
        },
        {
          label: '发布与运维',
          translations: { en: 'Release & operations' },
          collapsed: true,
          items: [{ autogenerate: { directory: 'release' } }],
        },
      ],
      components: {
        Header: './src/components/Header.astro',
        // Portal-parity brand lockup(见组件注释);starlight 默认 title 不像门户。
        SiteTitle: './src/components/SiteTitle.astro',
        // Search 不覆盖:让 starlight-versions 的版本过滤覆盖件接管。
        // 原 noWorker fork 已由 src/plugins/vite-pagefind-noworker.mjs 在
        // virtual:starlight/pagefind-config 上注入同样设置(组件所有权还给上游)。
        // Dark-only, mirroring the portal (见 ThemeProvider.astro)。
        ThemeProvider: './src/components/ThemeProvider.astro',
        ThemeSelect: './src/components/ThemeSelect.astro',
      },
      customCss: ['./src/styles/custom.css'],
      // 语法高亮:github-dark 的 bash 分色比默认 night-owl 完整(快速开始全
      // 是 shell 块),注释色对比 6.15:1。自定义 themes 会默认把
      // useStarlightUiThemeColors 翻成 false(custom.css 自证:门户 gray 系
      // 背景/边框全靠 Starlight UI 主题色),所以这里必须显式压回 true。
      expressiveCode: {
        themes: ['github-dark'],
        useStarlightUiThemeColors: true,
      },
    }),
  ],
});
