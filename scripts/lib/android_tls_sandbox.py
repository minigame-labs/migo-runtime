"""Bounded Linux builder/probe for the Android TLS helper.

Only selected read-only inputs and toolchains enter the namespace. No network,
home, credentials or repository writes are available to the target. The caller
may retain only bounded stdout, after validating its expected artifact format.
"""
import hashlib
import os
from pathlib import Path
import selectors
import shutil
import signal
import subprocess
import tempfile
import time

COMPILER_JARS = [
    ('org.jetbrains.kotlin', 'kotlin-compiler-embeddable', '2.0.21', 'compiler.jar'),
    ('org.jetbrains.kotlin', 'kotlin-stdlib', '2.0.21', 'compiler-stdlib.jar'),
    ('org.jetbrains.kotlin', 'kotlin-reflect', '2.0.21', 'compiler-reflect.jar'),
    ('org.jetbrains.intellij.deps', 'trove4j', '1.0.20200330', 'compiler-trove.jar'),
    ('org.jetbrains', 'annotations', '13.0', 'compiler-annotations.jar'),
    ('org.jetbrains.kotlinx', 'kotlinx-coroutines-core-jvm', '1.6.4', 'compiler-coroutines.jar'),
]
FLAGS = ('-XX:ActiveProcessorCount=2 -XX:+UseSerialGC -Xmx256m '
         '-XX:CompressedClassSpaceSize=64m -XX:ReservedCodeCacheSize=32m '
         '-Duser.home=/scratch/home -Djava.io.tmpdir=/scratch/tmp')
JDK = '/toolchain/jdk/bin/'


def options(parser):
    parser.add_argument('--gradle-cache', type=Path,
                        default=Path.home() / '.gradle/caches/modules-2/files-2.1')
    sdk = Path(os.environ.get('ANDROID_SDK_ROOT') or os.environ.get('ANDROID_HOME')
               or Path.home() / 'Android/Sdk')
    parser.add_argument('--android-jar', type=Path, default=sdk / 'platforms/android-36/android.jar')
    parser.add_argument('--jdk', type=Path, default=Path('/usr/lib/jvm/java-17-openjdk-amd64'))


def gradle_jar(cache, group, artifact, version):
    jars = list((cache / group / artifact / version).glob('*/*.jar'))
    if len(jars) != 1:
        raise ValueError(f'expected exactly one cached {group}:{artifact}:{version}, got {len(jars)}')
    return jars[0]


def inputs(args, compile_source):
    files = {'android.jar': args.android_jar.resolve(),
             'kotlin-stdlib.jar': gradle_jar(args.gradle_cache, 'org.jetbrains.kotlin',
                                           'kotlin-stdlib', '2.4.21')}
    if compile_source:
        for group, artifact, version, name in COMPILER_JARS:
            files[name] = gradle_jar(args.gradle_cache, group, artifact, version)
    return files


def hashes(files):
    return {name: hashlib.sha256(path.read_bytes()).hexdigest()
            for name, path in sorted(files.items())}


def compiler_script():
    javac_flags = ' '.join('-J' + flag for flag in FLAGS.split())
    compiler = ':'.join('/inputs/' + entry[3] for entry in COMPILER_JARS)
    return (
        f'{JDK}javac {javac_flags} --release 8 -g:none -d /scratch/buildconfig /inputs/BuildConfig.java\n'
        f"{JDK}java {FLAGS} -cp '{compiler}' org.jetbrains.kotlin.cli.jvm.K2JVMCompiler "
        '-no-stdlib -no-reflect -module-name migo_platform_tls -jvm-target 1.8 -Xlambdas=indy '
        '-classpath /inputs/android.jar:/inputs/compiler-stdlib.jar:/scratch/buildconfig '
        '-d /scratch/patched /inputs/CertificateVerifier.kt\n')


def run(files, script, jdk, *, output_limit=32768):
    jdk = jdk.resolve()
    if not (jdk / 'bin/javac').is_file():
        raise ValueError('JDK with javac is required')
    with tempfile.TemporaryDirectory(prefix='migo-android-tls-') as temporary:
        stage = Path(temporary)
        for name, source in files.items():
            if Path(name).name != name:
                raise ValueError('sandbox input names must be flat')
            if isinstance(source, bytes):
                (stage / name).write_bytes(source)
            else:
                shutil.copyfile(source, stage / name)
        (stage / 'BuildConfig.java').write_text(
            'package org.rustls.platformverifier; public final class BuildConfig { public static final boolean TEST=false; }\n')
        (stage / 'java.security').write_text(
            'security.provider.1=SUN\nsecurity.provider.2=SunRsaSign\nsecurity.provider.3=SunEC\n'
            'security.provider.4=SunJSSE\nkeystore.type=pkcs12\nsecurerandom.source=file:/dev/urandom\n')
        (stage / 'run.sh').write_text(
            'set -euo pipefail\nmkdir -p /scratch/home /scratch/tmp /scratch/classes\n' + script)
        command = ['bwrap', '--unshare-all', '--die-with-parent', '--new-session', '--cap-drop', 'ALL',
                   '--ro-bind', '/usr', '/usr', '--ro-bind', '/lib', '/lib', '--ro-bind', '/lib64', '/lib64',
                   '--dir', '/toolchain', '--ro-bind', str(jdk), '/toolchain/jdk',
                   '--dir', '/etc', '--ro-bind', '/etc/ld.so.cache', '/etc/ld.so.cache',
                   '--dir', '/etc/java-17-openjdk', '--ro-bind-try', '/etc/java-17-openjdk/jvm-amd64.cfg',
                   '/etc/java-17-openjdk/jvm-amd64.cfg', '--dir', '/etc/java-17-openjdk/security',
                   '--ro-bind', str(stage / 'java.security'), '/etc/java-17-openjdk/security/java.security',
                   '--ro-bind', str(stage), '/inputs', '--proc', '/proc', '--dev', '/dev',
                   '--size', '67108864', '--tmpfs', '/scratch', '--clearenv', '--setenv', 'PATH', '/usr/bin',
                   '--setenv', 'LANG', 'C.UTF-8', '--setenv', 'TMPDIR', '/scratch/tmp', '--chdir', '/scratch',
                   '/usr/bin/prlimit', '--cpu=45', '--as=4294967296', '--nproc=64', '--nofile=128',
                   '--fsize=1048576', '/usr/bin/bash', '/inputs/run.sh']
        process = subprocess.Popen(command, env={'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'},
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                   start_new_session=True)
        output = bytearray()
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + 60
        try:
            while selector.get_map():
                if time.monotonic() > deadline:
                    raise RuntimeError('Android TLS sandbox exceeded 60s wall limit')
                for key, _ in selector.select(timeout=0.2):
                    block = os.read(key.fd, 4096)
                    if not block:
                        selector.unregister(key.fd)
                    else:
                        output.extend(block)
                        if len(output) > output_limit:
                            raise RuntimeError('Android TLS sandbox exceeded output limit')
            status = process.wait(timeout=1)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
            selector.close()
        if status:
            raise RuntimeError(f'Android TLS sandbox exit {status}:\n'
                               + output.decode('utf-8', errors='replace'))
        return bytes(output)
