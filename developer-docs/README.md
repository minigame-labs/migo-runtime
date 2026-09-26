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
src/content/docs/            # 中文页面(当前版本,直接映射 /docs/ 路由)
  index.mdx                  # /docs/ 首页
  0.9/**/*.mdx               # 0.9.7 完整中文冻结归档(starlight-versions 生成,禁止手改)
  en/**/*.mdx                # 英文实译(latest);en/0.9/** 为完整英文冻结归档
src/content/versions/        # 归档版本的侧栏快照(0.9.json)
src/content/i18n/            # zh-CN 的 starlight-versions 文案
src/components/              # Header/SiteTitle/ThemeProvider/ThemeSelect 覆盖件
src/plugins/                 # vite-pagefind-noworker(Pagefind 不起 Worker,替代原 Search fork)
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

## 6. i18n、英文页面与归档

zh 是 root locale,内容在 `src/content/docs/`;en 页面在 `src/content/docs/en/`。latest 英文已于 2026-09-15 全量翻译并进 sitemap;sitemap filter 只排除两个归档树 `/docs/0.9/` 与 `/docs/en/0.9/`。

0.9.7 的中英文归档各有 43 页,与 latest 保持同一套路由和 C ABI 函数节。归档是发布基线,不再作为日常编辑目标;phase1 契约会阻止缺页、英文占位、跨出归档的内部链接或 API 节不一致。

**英文实译页的站内链接必须写 `/docs/en/…`**:Starlight 不改写正文里的绝对链接,写成中文路由就把英文读者第一次点击送进中文页。phase1 契约 §5b 拦截;参考页的 `## migo_*` 函数节还必须与中文页逐页一致(§7)。

zh 页面新建后,如果英文翻译没完成,运行 `npm run docs:sync-en` 生成同路径占位页,模板:

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

不要把中文正文伪装成英文翻译;占位页必须带 `noindex` 和回链。翻译完成后替换正文(代码块注释一并译成英文)、去掉上面的 aside 与 noindex,把站内链接改成 `/docs/en/…`。

## 7. 主题与站点导航

Portal 是纯深色,所以文档也是深色锁定(`ThemeProvider.astro` + `ThemeSelect.astro`,后者只渲染版本切换、不出主题切换)。跨站链接(`Header.astro` 顶栏:首页/开发者文档/性能数据/常见问题/SDK 下载)指向 `https://minigame-labs.com` 的绝对地址,改 Portal 导航时这里同步改。**文档面不放门户式 Footer**:页尾只留 starlight 默认的 pager —— 门户 footer 的法律/仓库信息在文档里没有读者收益(2026-09-15 删除 `Footer.astro` 覆盖件)。品牌色只允许 Migo 红 `#e5352c` 一个 accent(`custom.css` 的 `--sl-color-accent`)。品牌 lockup 是单一资产 `public/brand/lockup-on-dark.svg` —— 与 Portal `/brand/` 保持同步,两者来自同一个几何源。

## 8. 版本归档与发布新系列

当前稳定 SDK 是 0.9.7。`/docs/` 与 `/docs/en/` 服务当前稳定内容;`/docs/0.9/` 与 `/docs/en/0.9/` 是同一发布基线的完整冻结快照,用于固定链接和后续升级对照。`starlight-versions`、版本切换器、版本搜索和 `versions` collection 均已启用。

0.9 归档于 2026-09-26 重新生成一次,以补齐此前缺失的页面和英文占位内容。此后禁止手工修订归档;需要勘误时先修 latest,再明确判断是否值得做带审计记录的历史勘误。

发布下一系列(例如 0.10)时:

1. 先发布 SDK,把 `release/VERSION` 更新为真实版本,再维护文档;不要让文档先宣告尚未发布的 API。
2. 在 `astro.config.mjs` 的 `starlightVersions()` 配置中把新归档放到 `versions` 数组头部,保留 0.9 历史项及准确 label。插件只应生成缺失归档一次。
3. 检查新归档的中英文路由集合、API 函数节和所有 `/docs/…` 内链都留在对应版本前缀;生成器可能不会改写 JSX 组件的 `href`,phase1 契约会对此失败。
4. 同步更新 sitemap filter、`migo-www` 必出路由契约和必要的 Caddy 路由规则;归档继续从 sitemap 排除,latest 保持可索引。
5. 运行 `npm run docs:check && npm run docs:build && npm run docs:smoke`,确认版本切换、版本搜索、中英文归档、404、无 JS 阅读和无障碍检查。
6. 先提交并推送 `migo`,取得不可变 commit SHA;再更新 `migo-www/docs/migo-docs-source.json` 的 `ref` 和 `expectedVersion`,最后构建、部署网站。

归档内容和侧栏由生成时的 latest 快照派生,因此信息架构调整必须在归档前完成。不要把分支名作为生产来源,也不要在归档生成后用批量替换继续“追平” latest。

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

Playwright 覆盖:路由矩阵、`/docs/next/` 必须 404、英文实译与未来占位页契约、完整 0.9 中英文归档、无 JS 可读性、搜索、移动端、axe 严重违规为零。

## 13. 常见失败

| 症状 | 原因与处理 |
| --- | --- |
| links-validator 报 31+ 无效链接 | 写了 Docusaurus 式相对文件链接(`./foo.mdx`);改成 `/docs/foo/` 路由形式。 |
| `npm run docs:dev` 字体/样式不变 | Vite 缓存了旧 `custom.css`;重启 dev server(Starlight 对 customCss 的 HMR 不可靠)。 |
| 页面 h1 字体没变 | Starlight 的页面标题 h1 在 `.sl-markdown-content` 外,选择器要含 `h1[id='_top']`。 |
| 0.9 归档页出现在 sitemap | sitemap `filter` 被改;恢复仅排除 `/docs/0.9/` 与 `/docs/en/0.9/` 的规则并跑 `docs:build`;不要排除 latest 英文页。 |
| `release/VERSION` 与期望不一致 | `migo-www/docs/migo-docs-source.json` 的 `expectedVersion` 未随 SDK 更新。 |
| 出现 `docusaurus` 字样的报错或引用 | 清理不彻底;phase1 契约第 6 节列了全部禁留文件。 |
