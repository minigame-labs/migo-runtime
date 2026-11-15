/*
 * The page owns an XComponent and stages the bundled content.
 *
 * `libraryname` is what makes ArkUI load libmigohost.so and hand its native
 * module the XComponent instance; the native side then registers the surface
 * callbacks. Without it the component renders an empty surface and the native
 * code never learns a window exists -- which looks exactly like a rendering bug
 * rather than a wiring one.
 *
 * The content ships inside the HAP as a rawfile and is copied into the app's
 * files directory on first run. It cannot be pushed there from outside: the
 * sandbox path is visible only to this process, and hdc runs as the unprivileged
 * `shell` user. Shipping it in the package is also what a real application does.
 */

import migohost from 'libmigohost.so';
import { common } from '@kit.AbilityKit';
import { fileIo } from '@kit.CoreFileKit';
import { display } from '@kit.ArkUI';

const CONTENT_ID: string = 'touchprobe';
const CONTENT_FILES: string[] = ['game.js', 'game.json'];

@Entry
@Component
struct Index {
  @State status: string = 'starting';

  /* Layout the engine expects: <filesDir>/migo/games/<id>/code/<entry> */
  private stageContent(filesDir: string): void {
    const codeDir: string = `${filesDir}/migo/games/${CONTENT_ID}/code`;
    // mkdir -p, one level at a time: fileIo has no recursive create.
    let acc: string = filesDir;
    for (const part of ['migo', 'games', CONTENT_ID, 'code']) {
      acc = `${acc}/${part}`;
      try {
        fileIo.mkdirSync(acc);
      } catch (_e) {
        // Already present on a second run; any real failure surfaces below when
        // the file write fails, with a path in the message.
      }
    }

    const ctx = getContext(this) as common.UIAbilityContext;
    for (const name of CONTENT_FILES) {
      const bytes: Uint8Array = ctx.resourceManager.getRawFileContentSync(`content/${name}`);
      const target: string = `${codeDir}/${name}`;
      const file = fileIo.openSync(target, fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.CREATE
        | fileIo.OpenMode.TRUNC);
      fileIo.writeSync(file.fd, bytes.buffer);
      fileIo.closeSync(file);
    }
    console.info(`[migo] staged ${CONTENT_FILES.length} content files`);
  }

  build() {
    Stack() {
      XComponent({
        id: 'migo_surface',
        type: XComponentType.SURFACE,
        libraryname: 'migohost'
      })
        .onLoad(() => {
          // Started here rather than in aboutToAppear: onLoad fires once the
          // native module is loaded and bound to this component, so the native
          // side is guaranteed to exist before it is called into.
          try {
            const ctx = getContext(this) as common.UIAbilityContext;
            const filesDir: string = ctx.filesDir;
            const cacheDir: string = ctx.cacheDir;
            const scaleFactor: number = display.getDefaultDisplaySync().densityPixels;
            this.stageContent(filesDir);
            const rc: number = migohost.start(filesDir, cacheDir, CONTENT_ID, scaleFactor);
            this.status = rc === 0 ? `engine started (${CONTENT_ID})` : `start failed: ${rc}`;
            console.info(`[migo] start -> ${rc}`);
          } catch (_e) {
            this.status = 'content staging failed';
            console.error('[migo] content staging failed');
          }
        })
        .onDestroy(() => {
          // Stop is release-aware: -12 means the native release observer is
          // still pending and its callback will finish teardown asynchronously.
          const rc: number = migohost.stop();
          if (rc !== 0 && rc !== -12) {
            console.error(`[migo] stop failed: ${rc}`);
          }
        })
        .width('100%')
        .height('100%')

      Text(this.status)
        .fontSize(14)
        .fontColor(Color.White)
        .backgroundColor('#80000000')
        .padding(6)
        .position({ x: 8, y: 8 })
    }
    .width('100%')
    .height('100%')
  }
}
