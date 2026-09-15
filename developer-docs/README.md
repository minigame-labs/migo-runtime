# Migo 开发者文档维护手册

本文档说明如何在本地维护开发者文档、发布新版本,并把站点发布到 `migo-www`。所有命令默认在 `developer-docs/` 目录执行;Node.js 版本以 `package.json` 的 `engines` 为准(Astro 7 要求 ≥ 22.12,本仓库统一钉在 ≥ 22.19,与门户一致)。

技术栈:**Astro 7 + @astrojs/starlight**。搜索用 Starlight 内置的 Pagefind,内部链接由 `starlight-links-validator` 在构建时校验,Mermaid 由 `astro-mermaid` 客户端渲染。构建产物输出到 `build/`(不要在 Astro 配置里改这个目录,migo-www 依赖它)。

## 1. 本地安装与开发服务器

```bash
cd developer-docs
npm ci
npm run docs:check
npm run docs:dev   # http://127.0.0.1:8097/docs/
```

`docs:dev` 支持热更新。首页对英文浏览器会客户端跳转到 `/docs/en/`(Starlight 的语言协商);验证中文首页时直接请求 `http://127.0.0.1:8097/docs/` 看 HTML,或把浏览器语言切成中文。

预览生产构建:

```bash
npm run docs:build   # astro build + scripts/check-built-site.mjs
npm run docs:serve
```

`docs:build` 分为两步:`docs:build:site`(astro build)和 `docs:check:built`(产物契约)。两者都在 `npm run build` 管线的必经路径上;单独 debug 时也可分开跑。

## 2. 目录结构

```
src/content/docs/            # 中文页面(latest,直接映射 /docs/ 路由)
  index.mdx                  # /docs/ 首页
  en/**/*.mdx                # 英文占位页(Translation pending)
src/components/              # Header/SiteTitle/Search/ThemeProvider/ThemeSelect 覆盖件
src/styles/custom.css        # 品牌层:SmileySans 标题字、Portal 红色 accent
docs.config.mjs              # release/VERSION → docsSeries 的唯一来源(被 astro.config 的 head 用作 docs-version meta)
astro.config.mjs             # base、locale、sidebar、插件
public/                      # 静态资源(brand/fonts/img),URL = /docs/<path>
scripts/                     # check-doc-metadata.mjs / check-built-site.mjs / ensure-en-stubs.mjs(en stub 生成+断言)
tests/docs.spec.ts           # Playwright + axe
```

## 3. 添加或修改中文 MDX

页面位于 `src/content/docs/`,**文件名即路由**(目录 `foo/bar.mdx` → `/docs/foo/bar/`);首页必须是 `index.mdx`,不要再写 `slug: /`。frontmatter 必填 `title`、`description`;可选项目自定义字段 `audience`、`platforms`、`sourcePaths`(schema 在 `src/content.config.ts` 里 extend)。站内链接一律用路由形式 `/docs/reference/types/`,**不要**写 `./types.mdx` —— links-validator 在构建期会拦下。

新页面登记到 `astro.config.mjs` 的 `sidebar`(带 `translations.en` 标签)。代码示例必须可运行,命令从读者实际所在目录写起。

## 4. `sourcePaths` 使用规则

`sourcePaths` 指向运行时仓库里的规范源(如 `include/migo/migo.h`),`scripts/check-doc-metadata.mjs` 校验每个路径真实存在:

- 路径相对仓库根目录,必须真实存在;
- 优先引用唯一的规范源,避免同一页面出现两份可编辑副本;
- 修改头文件后跑 `npm run docs:check` 重新验证。

## 5. Mermaid 与文本等价说明

Mermaid 图不能是唯一信息来源(客户端渲染,禁 JS 时不出现)。每幅图后必须紧跟 `### 文字说明`,用有序列表或表格说清节点、方向、分支和失败路径。phase1 契约强制检查这一条。

## 6. i18n 与英文占位页

zh 是 root locale,内容在 `src/content/docs/`;en 页面在 `src/content/docs/en/`,`astro.config.mjs` 的 sitemap filter 已把 `/docs/en/` 排除。

zh 页面新建后,如果英文翻译没完成,复制同路径占位页到 `en/`,模板:

```mdx
---
title: <英文标题>
description: <英文一句话描述>
head:
  - tag: meta
    attrs:
      name: robots
      content: noindex
# 项目自定义字段照抄 zh 页面
---

:::note[Translation pending]
This page is pending translation. The 中文版本 (link below) is the current source of truth.
:::

[→ 中文版本](/docs/<同路径>/)
```

不要把中文正文伪装成英文翻译;占位页必须带 `noindex` 和回链。翻译完成后替换正文、去掉上面的 aside 与 noindex,并在 sitemap filter 放行(先把 `filter` 里 `/docs/en/` 条件去掉,再检查 sitemap)。

## 7. 主题与站点导航

Portal 是纯深色,所以文档也是深色锁定(`ThemeProvider.astro` + 空 `ThemeSelect.astro`)。跨站链接(`Header.astro` 顶栏:首页/开发者文档/性能数据/常见问题/SDK 下载)指向 `https://minigame-labs.com` 的绝对地址,改 Portal 导航时这里同步改。**文档面不放门户式 Footer**:页尾只留 starlight 默认的 pager —— 门户 footer 的法律/仓库信息在文档里没有读者收益(2026-09-15 删除 `Footer.astro` 覆盖件)。品牌色只允许 Migo 红 `#e5352c` 一个 accent(`custom.css` 的 `--sl-color-accent`)。品牌 lockup 是单一资产 `public/brand/lockup-on-dark.svg` —— 与 Portal `/brand/` 保持同步,两者来自同一个几何源。

## 8. 发布新版本(0.10 时读这里)

当前只有 0.9 一个版本,`/docs/` 根路径直接服务 latest stable,URL 不含版本号。IA 见 `docs/IA.md`(链接待移入;当前 spec 于 `migo-all/migo-www/docs/superpowers/specs/2026-09-14-docs-ia.md`)。**0.10 发布时**按下面做:

1. 确认 `release/VERSION` 已切到 `0.10.0` 且 SDK 已发布(文档永远后于 SDK,见 §10)。
2. **先装回依赖**(已于 2026-09-14 从 deps 移除,与启用同 PR 才获得供应链审查):`npm i starlight-versions@^0.10.1`;在 `astro.config.mjs` 的 plugins 里启用:
   ```js
   starlightVersions({ versions: [{ slug: '0.9' }] }),
   ```
   并在 `src/content.config.ts` 加回 `versions` collection(见文件内注释)。
3. 运行 `npx astro dev` 一次 —— 插件会把当前 `src/content/docs/` 的 0.9 状态归档到 `src/content/docs/0.9/` 和 `src/content/versions/0.9.json`;根目录继续承载 0.10 工作稿。
4. 全量 `npm run docs:check && npm run docs:build`,确认 `/docs/0.9/` 可访问、根路径是新内容。
5. 更新 `migo-all/migo-www/scripts/check-built-site.mjs` 的必出路由表(加 `docs/0.9/index.html`)。
6. **版本组件会被覆盖件吞(必然现在已知):** `Search`(noWorker fork)与 `ThemeSelect`(dark-only)覆盖会让插件的版本组件只 warn 不接管 → 版本切换入口不出现 + Pagefind 的 `version:` 过滤失效。启用版本化必须在同一个 PR 里把手做:ThemeSelect 内部手动渲染 `VersionSelect.astro`;`Search.astro` fork 的 noWorker 逻辑合进 `VersionSearch.astro` 副本（三方合并)。
7. sitemap/canonical/Caddy 与归档同一 PR:`/docs/0.9/**` 归档后出现就是明确的重复内容。以 IA spec §6.4 清单为准:sitemap filter 同步排除 `/docs/0.9/**`(en 排除是当前已有),Caddy `docsSearch`/`docs_astro` 规则按新的归档目录形状补(portal 端的部署资产算 IA spec 的正式配套)。

**IA 定稿必须先于 0.10 归档**(spec §6.2):归档是冻结的且侧栏从当前 config 前缀化派生,归档之后做 IA 改造 = latest 与 0.9 永久分叉。也不要先在 `astro.config.mjs` 里全开「宿主能力」组再归档 - 未被验证( spec §6.5 )。

旧版本归档是冻结的,禁止手工改 `src/content/docs/0.9/` 修字。

## 9. `release/VERSION` 如何决定 `docsSeries`

`release/VERSION` 是唯一版本来源。`docs.config.mjs` 读它并计算 `docsSeries`(major.minor);`astro.config.mjs`、`migo/scripts/test-developer-docs-phase1-contract.sh` 都从这里取数。任何文件里出现第二个硬编码版本号都是 bug。

## 10. 文档与 SDK 的发布顺序

先发布 SDK 及其 `release/VERSION`,再让文档跟进,最后发布网站。网站发布前确认 `migo-www/docs/migo-docs-source.json` 的 `expectedVersion` 与 SDK 一致。不要把未发布的 API 提前写成稳定内容。

## 11. `migo-www` 如何获取并构建文档

`migo-www/docs/migo-docs-source.json` 指定仓库、`ref` 和 `expectedVersion`。`npm run build:docs` 会临时浅克隆该 ref,校验 `release/VERSION`,执行 `npm ci --ignore-scripts`、`docs:check`(lint + 契约 + astro check + metadata)、`docs:build`,然后复制 `build/` 到 `dist/docs/`。本地联调:

```bash
MIGO_DOCS_SOURCE_DIR=/绝对路径/migo npm run build:docs
```

生产部署前把 `ref` 固定为不可变 commit SHA 或 release tag;`main` 只适合初始引导。

## 12. 完整检查与 Playwright

```bash
npm ci
npm run docs:check     # markdownlint + phase1 契约 + astro check + metadata
npm run docs:build
npm run docs:serve &   # 另开终端
npm run docs:smoke     # playwright(需要本机 Chrome,CHROME_PATH 可覆盖)
```

Playwright 覆盖:路由矩阵、`/docs/next/` 必须 404、en 占位页 noindex + 回链、无 JS 可读性、搜索、移动端、axe 严重违规为零。

## 13. 常见失败

| 症状 | 原因与处理 |
| --- | --- |
| links-validator 报 31+ 无效链接 | 写了 Docusaurus 式相对文件链接(`./foo.mdx`);改成 `/docs/foo/` 路由形式。 |
| `npm run docs:dev` 字体/样式不变 | Vite 缓存了旧 `custom.css`;重启 dev server(Starlight 对 customCss 的 HMR 不可靠)。 |
| 页面 h1 字体没变 | Starlight 的页面标题 h1 在 `.sl-markdown-content` 外,选择器要含 `h1[id='_top']`。 |
| en 页在 sitemap 里出现 | sitemap `filter` 被改;恢复 `!page.includes('/docs/en/')` 并跑 `docs:build`。 |
| `release/VERSION` 与期望不一致 | `migo-www/docs/migo-docs-source.json` 的 `expectedVersion` 未随 SDK 更新。 |
| 出现 `docusaurus` 字样的报错或引用 | 清理不彻底;phase1 契约第 6 节列了全部禁留文件。 |
