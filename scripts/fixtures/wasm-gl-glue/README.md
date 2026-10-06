# 这个 fixture 是为了回答哪个问题

MigoGLX（P3-G）的开始条件有两条：拿到真实 Unity/Emscripten 导出，**且**实测 JS glue
是热点。计划里写着「全仓一个 `.wasm` 都没有，今天连热点都测不出来」——这就是那个缺口。

`src/main.c` 是自己写的 Emscripten WebGL2 程序（不需要 Unity 授权、不用别人的游戏），
每帧发 2000 次小 draw、每次带两个 uniform 更新，刻意做成 glue 密集的形状。

## ★ 已有的 profiling 数据回答不了这个问题

`docs`/memory 里有一条现成结论：三类游戏的瓶颈类型学中 **WebGL 类是 JS/IC-bound**。
**不能拿它当「glue 是热点」的证据**——那批内容（Pixi / Phaser）是**手写 JS**，
根本没有 WASM→JS glue 这一层。它的热点在 JS 引擎自身。

MigoGLX 针对的是 **Emscripten 导出**：JS glue 是 WASM 和 WebGL 之间那层薄封装，
是一条独立的路径。所以这个测量必须单独做，旧数据既不能证实也不能证伪。

## 这个 fixture 能给出什么、不能给出什么

- **能**：WASM→JS→op 这条路上**每次 GL 调用的成本**。
- **不能**：「glue 是不是热点」。那取决于**真实内容的调用次数**，而这个 fixture 是
  刻意做密的，比值会偏向「glue 重要」。

正确用法是：拿这里的每调用成本 × 真实内容的每帧调用数，再和帧预算比。
现有的 bunnymark / endless-runner 已经能给出后者的量级。

## 2026-09-02 的进展与两个先于性能的发现

emsdk 装成过一次，`src/main.c` **编译成功并在 Migo 上跑到了 `main()`**
（13.6 KB wasm + 26.4 KB glue）。在拿到 glue 性能数字之前，先撞上两件事——
**它们都不是 MigoGLX 能解决的问题，而是任何 Unity/Emscripten 导出会首先遇到的兼容地板**：

1. **Emscripten 用 `fetch` / `XHR` 加载自己的 `.wasm`，Migo 两个都没有。**
   原样导出直接 abort：`both async and sync fetching of the wasm failed`。
   本 fixture 用 `-sSINGLE_FILE=1` 绕开（wasm 内嵌成 data URI）——**这对测每调用成本
   没问题，对测启动是错的**：base64 解码取代了流式编译。
2. **Emscripten 用 `document.querySelector('#canvas')` 找渲染目标，Migo 没有 DOM。**
   但 Migo 的 canvas 暴露了 `getContext`，而那正是 Emscripten GL 层唯一真正调用的东西，
   所以一个两方法的 `document` 替身就够（见 `pre.js`）。

### 四层地板里，只有一层是引擎的

排查到底一共四层，**但它们的归属不同**：

| # | 地板 | 归属 |
|---|---|---|
| 1 | `fetch`/`XHR` 载 `.wasm` | **adapter**（`migo.*` 是引擎唯一能力面，网络 API 属 adapter） |
| 2 | `document.querySelector('#canvas')` | **adapter**（引擎里没有也不该有 `document`） |
| 3 | Emscripten 判 Migo 为 "shell" 环境 | **构建参数**（`-sENVIRONMENT=web,shell`），双方都不用改 |
| 4 | `gl2 instanceof WebGLRenderingContext` 返回 `true` | 🔴 **引擎**——违反 WebIDL，已修 |

前三层是「宿主要给什么」，第四层是「引擎给错了什么」。只有第四层是缺陷，
已修并由 `migo-conformance/tests/webgl-context-identity` 钉住（真机 92/92）。

**这个次序对项目决策有意义**：MigoGLX 是关于 glue 有多快的；而在它之前，
导出连载入和拿到画布都做不到。先解决地板，再谈天花板。

之后 emsdk 的续装与 API 33 镜像都被网络反复打断（emsdk 三次、镜像两次），
`upstream/` 被清空且下载缓存已删，当晚未能重建。**测量本身没有做完**，
装置、shim 与上述两条发现都已就位，网络正常的会话可以直接接着跑。

## 构建

```bash
source ~/emsdk/emsdk_env.sh
emcc src/main.c -o game.js -sMAX_WEBGL_VERSION=2 -sMIN_WEBGL_VERSION=2 \
     -sALLOW_MEMORY_GROWTH=1 -O2
```
