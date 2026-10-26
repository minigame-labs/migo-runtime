# Public SDK ABI and JNI class names must survive the consuming app's R8 pass.
-keep,allowoptimization public class com.migo.runtime.* { public protected *; }
-keep,allowoptimization public interface com.migo.runtime.* { public protected *; }
-keep,allowoptimization public enum com.migo.runtime.* { public protected *; }
-keep,allowoptimization public class com.migo.runtime.callback.** { public protected *; }
-keep,allowoptimization public interface com.migo.runtime.callback.** { public protected *; }
-keep,allowoptimization public enum com.migo.runtime.callback.** { public protected *; }
# Rust reads RuntimeConfig's fields by name (JNI GetFieldID), and they are
# private: the rules above keep only public/protected members, so a minifying
# app would rename or strip them and every session would fail to start.
-keepclassmembers class com.migo.runtime.RuntimeConfig {
    <fields>;
}
-keepnames class com.migo.runtime.internal.NativeBridge
-keepnames class com.migo.runtime.internal.NativeExports
# Native session initialization obtains the application Context for TLS.
-keep class com.migo.runtime.internal.AppContext {
    public static android.content.Context getOrNull();
}
# rustls calls these three upstream classes/members by JNI names.
-keep class org.rustls.platformverifier.** { *; }
-keepattributes *Annotation*,Signature,InnerClasses,EnclosingMethod
