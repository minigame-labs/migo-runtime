"""Native dependency-closure controls, compiled with the gate's actual NDK."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main(ndk):
    root = Path(__file__).resolve().parents[2]
    tools = ndk / "toolchains/llvm/prebuilt/linux-x86_64/bin"
    environment = os.environ.copy()
    environment["ANDROID_NDK_HOME"] = str(ndk)
    with tempfile.TemporaryDirectory(prefix="migo-android-floor-") as temporary:
        work = Path(temporary)
        provider = work / "provider.c"
        consumer = work / "consumer.c"
        provider.write_text("#include <unistd.h>\nint migo_provider(void) { return getpid(); }\n")
        consumer.write_text("extern int migo_provider(void);\nint migo_entry(void) { return migo_provider(); }\n")

        def compile(source, output, *flags, arch="aarch64"):
            subprocess.run([str(tools / f"{arch}-linux-android26-clang"), "-shared", "-fPIC",
                            str(source), *flags, "-o", str(output)], check=True)

        def build_provider(*, soname="libprovider.so", arch="aarch64"):
            compile(provider, work / "libprovider.so", f"-Wl,-soname,{soname}", arch=arch)

        def check(expected, message):
            result = subprocess.run(["bash", str(root / "scripts/test-android-symbol-floor.sh"),
                                     str(work / "libconsumer.so")], env=environment,
                                    capture_output=True, text=True)
            output = result.stdout + result.stderr
            assert (result.returncode == 0) == expected, output
            assert message in output, output

        build_provider()
        compile(consumer, work / "libconsumer.so", f"-L{work}", "-lprovider", "-Wl,-soname,libconsumer.so")
        check(True, "2 bundled ELF file(s)")
        (work / "libprovider.so").unlink()
        check(False, "missing bundled dependency")
        build_provider(arch="x86_64")
        check(False, "wrong ELF machine")
        build_provider(soname="libwrong.so")
        check(False, "SONAME differs")
        provider.write_text("extern int migo_missing_api(void);\nint migo_provider(void) { return migo_missing_api(); }\n")
        build_provider()
        # An unrelated sibling exporting the missing name cannot satisfy the
        # import: it is absent from the actual DT_NEEDED graph.
        stray = work / "stray.c"
        stray.write_text("int migo_missing_api(void) { return 0; }\n")
        compile(stray, work / "libstray.so", "-Wl,-soname,libstray.so")
        check(False, "migo_missing_api")
        # A real newer platform import carries an ELF version in r30. It must
        # remain refused after matching default @@ definitions to @ imports.
        provider.write_text("#include <sys/random.h>\nint migo_provider(void) { char byte; return getrandom(&byte, 1, 0); }\n")
        subprocess.run([str(tools / "aarch64-linux-android28-clang"), "-shared", "-fPIC",
                        str(provider), "-Wl,-soname,libprovider.so", "-o", str(work / "libprovider.so")], check=True)
        check(False, "getrandom@LIBC_P")
        print("Android symbol floor: actual API26 closure plus missing/transitive/architecture/SONAME/stray-export controls PASS")


if __name__ == "__main__":
    main(Path(sys.argv[1]).resolve())
