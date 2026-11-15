import { AbilityConstant, ConfigurationConstant, UIAbility, Want } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';
import migohost from 'libmigohost.so';

const DOMAIN = 0x0000;

export default class EntryAbility extends UIAbility {
  private stopRetryId: number = -1;
  private stopAttempts: number = 0;

  private setHostForeground(foreground: boolean): void {
    const rc: number = migohost.setForeground(foreground);
    if (rc !== 0) {
      hilog.error(DOMAIN, 'migo', 'Native foreground transition failed: %{public}d', rc);
    }
  }

  private requestHostStop(): void {
    const rc: number = migohost.stop();
    if (rc === 0) {
      if (this.stopRetryId >= 0) {
        clearTimeout(this.stopRetryId);
        this.stopRetryId = -1;
      }
      this.stopAttempts = 0;
      return;
    }
    if (rc === -12 && this.stopAttempts < 40 && this.stopRetryId < 0) {
      this.stopAttempts++;
      this.stopRetryId = setTimeout(() => {
        this.stopRetryId = -1;
        this.requestHostStop();
      }, 50);
      return;
    }
    if (rc !== -12 || this.stopAttempts >= 40) {
      hilog.error(DOMAIN, 'migo', 'Native stop failed or timed out: %{public}d', rc);
    }
  }

  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    try {
      this.context.getApplicationContext().setColorMode(ConfigurationConstant.ColorMode.COLOR_MODE_NOT_SET);
    } catch (_err) {
      hilog.error(DOMAIN, 'migo', 'Failed to set the color mode');
    }
    hilog.info(DOMAIN, 'testTag', '%{public}s', 'Ability onCreate');
  }

  onDestroy(): void {
    this.requestHostStop();
    hilog.info(DOMAIN, 'testTag', '%{public}s', 'Ability onDestroy');
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    // Main window is created, set main page for this ability
    hilog.info(DOMAIN, 'testTag', '%{public}s', 'Ability onWindowStageCreate');

    windowStage.loadContent('pages/Index', (err) => {
      if (err.code) {
        hilog.error(DOMAIN, 'migo', 'Failed to load the main page: %{public}d', err.code);
        return;
      }
      hilog.info(DOMAIN, 'testTag', 'Succeeded in loading the content.');
    });
  }

  onWindowStageDestroy(): void {
    // Main window is destroyed, release UI related resources
    this.requestHostStop();
    hilog.info(DOMAIN, 'testTag', '%{public}s', 'Ability onWindowStageDestroy');
  }

  onForeground(): void {
    // Ability has brought to foreground
    this.setHostForeground(true);
    hilog.info(DOMAIN, 'testTag', '%{public}s', 'Ability onForeground');
  }

  onBackground(): void {
    // Ability has back to background
    this.setHostForeground(false);
    hilog.info(DOMAIN, 'testTag', '%{public}s', 'Ability onBackground');
  }
}
