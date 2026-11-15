# 商业许可 / Commercial Licensing

[English](#english) · 中文

Migo 以 [Migo Binary License 1.0](LICENSE) 二进制分发。**多数接入方不需要商业许可**。本页说明哪些情形需要、哪些不需要，以及如何联系。

---

## 不需要商业许可的情形

以下用途由 LICENSE 直接授予，免费，无需联系，无需签署任何文件：

- **开发、构建、测试、评估与性能测试**：不限规模，不限主体（LICENSE §2.1）。
- **在自己的 App 中嵌入 Migo 并上线**，前提是属于 LICENSE 定义的 **Small Entity**（§2.2）：
  - 主体及其关联方最近一个完整财年的**全部**营业收入不超过 **USD 1,000,000**；**并且**
  - 该财年内任一月份，App 全部实例合计的**月活跃用户不超过 3,000,000**。
- **开发、发行、销售运行在 Migo 上的小游戏**：游戏本身的收入不受影响（§2.5）。
- **在自己的 App 中运行第三方开发的小游戏**：包括在 App 内经营小游戏平台、向游戏开发者提供接入工具与文档，只要 Migo 本身仍嵌在自己的 App 中、不提供给这些开发者（§2.5）。

超过阈值后，原有的上线使用**继续受许可覆盖 90 天**，用于签订商业许可，线上服务不会中断。

## 需要商业许可的情形

1. **不再属于 Small Entity**，但仍要在自己的 App 中嵌入 Migo 上线。
2. **把 Migo 作为独立的 SDK、引擎、框架或运行时组件提供给第三方**，无论是否与其他代码打包。
3. **向第三方提供托管、云端或代运营的小游戏运行时服务**，且核心运行能力来自 Migo。

第 2、3 种情形与规模无关，属于 LICENSE 中的 Competitive Offering（§3(a)）。

## 商业许可包含的内容

- 按实际分发形态（嵌入自有 App / 平台内运行第三方游戏 / 车机与电视等设备预装）书面授权。
- 明确的版本与升级范围，以及可预期的停止维护时间。
- 可选：**机型适配与支持 SLA**。Migo 的正确性来自在真实机型与 ROM 上逐一排查的问题，这项服务把这些积累直接用于接入方的目标机型。
- 可选：在保密协议下审阅源码，用于安全评审或供应商准入。
- 可选：兼容性认证、内容签名与分发、定制构建。

报价依分发形态、规模区间与支持等级确定，逐项商定。初次沟通通常只需要三项信息：**分发形态、预计规模、目标机型范围**。

## 联系方式

将上述三项信息发送至 **licensing@minigame-labs.com**。涉及保密信息时，可先签署保密协议。

安全问题请勿经由本渠道报告，见 [SECURITY.zh-CN.md](SECURITY.zh-CN.md)。

## 相关文件

- [LICENSE](LICENSE)：具有约束力的完整条款，本页与其不一致时以 LICENSE 为准。
- [LEGAL.md](LEGAL.md)：许可、商标、第三方组件与测试内容的法律说明。
- [NOTICE](NOTICE)：第三方组件及其许可证。

---

<a name="english"></a>

# Commercial Licensing

Migo is distributed in binary form under the [Migo Binary License 1.0](LICENSE). **Most integrators never need a commercial license.** This page states who does, who does not, and how to start the conversation.

## You do **not** need a commercial license

Granted directly by the LICENSE — free, no contact, nothing to sign:

- **Developing, building, testing, evaluating and benchmarking**, at any scale, by any entity (LICENSE §2.1).
- **Shipping Migo inside your own app**, while you are a **Small Entity** as defined in the LICENSE (§2.2):
  - total annual gross revenue of the entity and its affiliates, from all sources, of no more than **USD 1,000,000** in the most recently completed fiscal year; **and**
  - no more than **3,000,000 monthly active users** across all instances of your app in any single month of that year.
- **Developing, publishing and selling mini-games that run on Migo.** Revenue from the games themselves is unaffected (§2.5).
- **Running third-party mini-games inside your own app**, including operating a mini-game platform in your app and giving game developers tools and documentation to target it, as long as Migo itself stays embedded in your app and is not provided to those developers (§2.5).

If you cross a threshold, your existing production use stays covered for **90 days** so you can arrange a license without interrupting your service.

## You **do** need a commercial license

1. **You are no longer a Small Entity** but still want to ship Migo inside your own app.
2. **You want to provide Migo to third parties as a standalone SDK, engine, framework or runtime component**, bundled with other code or not.
3. **You want to offer a hosted, cloud or managed mini-game runtime service** whose core runtime functionality comes from Migo.

Cases 2 and 3 are independent of size: they are a Competitive Offering under LICENSE §3(a).

## What a commercial license covers

- A written grant scoped to how you actually distribute (embedded in your own app / third-party games on your platform / pre-installed on vehicles, TVs or other devices).
- Defined version and upgrade scope, with a predictable end-of-support date.
- Optional: **device bring-up and a support SLA**. Migo is correct because of every problem tracked down on real handsets and real ROMs; this points that accumulated work at your device matrix.
- Optional: source review under NDA, for a security review or vendor qualification.
- Optional: compatibility certification, content signing and distribution, custom builds.

Pricing is quoted per deal, based on distribution shape, scale band and support tier. A first conversation usually needs only three facts: **how you distribute, what scale you expect, and which devices you target.**

## How to start

Email **licensing@minigame-labs.com** with those three points. An NDA can be put in place first if needed.

Please do not use this channel for security reports — see [SECURITY.md](SECURITY.md).

## Related

- [LICENSE](LICENSE) — the binding terms; it governs wherever this page differs.
- [LEGAL.md](LEGAL.md) — licensing, trademark, third-party components and test content.
- [NOTICE](NOTICE) — third-party components and their licenses.
