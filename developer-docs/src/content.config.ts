import { defineCollection } from 'astro:content';
import { z } from 'astro/zod';
import { docsLoader, i18nLoader } from '@astrojs/starlight/loaders';
import { docsSchema, i18nSchema } from '@astrojs/starlight/schema';
import { docsVersionsLoader } from 'starlight-versions/loader';
export const collections = {
  docs: defineCollection({
    loader: docsLoader(),
    schema: docsSchema({
      extend: z.object({
        audience: z.string().optional(),
        platforms: z.array(z.string()).optional(),
        sourcePaths: z.array(z.string()).optional(),
      }),
    }),
  }),
  // starlight-versions 0.10.1 active (enabled 2026-09-15 by user decision).
  // 归档已实现:src/content/docs/0.9/** + src/content/versions/0.9.json。
  versions: defineCollection({ loader: docsVersionsLoader() }),
  // starlight-versions 只带 en/de/es — zh-CN 的 starlightVersions.* 键在这里覆盖,
  // 走 starlight 原生 i18n collection(§ src/content/i18n/zh-CN.json)。
  i18n: defineCollection({ loader: i18nLoader(), schema: i18nSchema() }),
};
