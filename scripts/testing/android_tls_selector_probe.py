#!/usr/bin/env python3
"""Check JCA factory selection only; this is not an Android trust/pinning test."""
import argparse
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import android_tls_sandbox as sandbox


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument('--jar', type=Path)
    source.add_argument('--source', type=Path)
    parser.add_argument('--expected', choices=['explicit_keystore', 'default_host_config'], required=True)
    parser.add_argument('--factories-per-thread', action='store_true')
    sandbox.options(parser)
    args = parser.parse_args()
    files = sandbox.inputs(args, bool(args.source))
    files['TrustManagerProbe.java'] = Path(__file__).with_name('TrustManagerProbe.java')
    script = ''
    if args.source:
        files['CertificateVerifier.kt'] = args.source
        script += sandbox.compiler_script()
        classes = '/scratch/patched'
    else:
        files['classes.jar'] = args.jar
        classes = '/inputs/classes.jar'
    javac_flags = ' '.join('-J' + flag for flag in sandbox.FLAGS.split())
    ownership = ' per_thread_factory' if args.factories_per_thread else ''
    script += (f'{sandbox.JDK}javac {javac_flags} --release 8 -d /scratch/classes /inputs/TrustManagerProbe.java\n'
               f'{sandbox.JDK}java {sandbox.FLAGS} -cp /scratch/classes:{classes}:'
               f'/inputs/kotlin-stdlib.jar:/inputs/android.jar TrustManagerProbe {args.expected}{ownership}\n')
    print(sandbox.run(files, script, args.jdk).decode(), end='')


if __name__ == '__main__':
    main()
