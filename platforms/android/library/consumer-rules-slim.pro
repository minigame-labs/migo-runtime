# Core JNI roots for the consuming app's R8 pass. Keep this list in lockstep
# with proguard-slim.pro and jni/profile_contract.rs.
-keepclassmembers,allowoptimization class com.migo.runtime.internal.NativeBridge {
    static native *** version(...);
    static native *** getMinApiLevel(...);
    static native *** initIcuData(...);
    static native *** init(...);
    static native *** shutdown(...);
    static native *** onShow(...);
    static native *** onHide(...);
    static native *** onRestart(...);
    static native *** updateSurface(...);
    static native *** onSurfaceDestroyed(...);
    static native *** onTouchEvent(...);
    static native *** modMain(...);
    static native *** executeScript(...);
    static native *** onVsync(...);
    static native *** getDebugStats(...);
    static native *** getConsoleLogs(...);
    static native *** nativeAhbPointerFromHardwareBuffer(...);
    static native *** onKeyboardInput(...);
    static native *** onKeyboardConfirm(...);
    static native *** onKeyboardComplete(...);
    static native *** onKeyboardHeightChange(...);
    static native *** onMemoryWarning(...);
    static native *** onThermalStatusChanged(...);
    static native *** onSubpackageProgress(...);
    static native *** onSubpackageResult(...);
}
-keepclassmembers,allowoptimization class com.migo.runtime.internal.NativeExports {
    public static *** beginRuntimeRestart(...);
    public static *** completeRuntimeRestart(...);
    public static *** unzipFile(...);
    public static *** encodeGbk(...);
    public static *** decodeGbk(...);
    public static *** decodeImageAhb(...);
    public static *** decodeImageRgba(...);
    public static *** keyboardShow(...);
    public static *** keyboardHide(...);
    public static *** keyboardUpdate(...);
    public static *** subpackageDownload(...);
    public static *** onGameReady(...);
    public static *** requestVsync(...);
    public static *** onError(...);
    public static *** onExit(...);
    public static *** onHostMessage(...);
    public static *** onSurfaceLost(...);
}
