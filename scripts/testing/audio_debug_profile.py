"""Keep paced probes' debug codegen faithful to the engine workspace.

An isolated manifest lost dev dependency O2; copying only the wildcard would
instead optimize IO/services/shared that are workspace members in the engine.
Preserve both roles, and verify actual Cargo artifacts before timing playback.
Release probes retain their explicitly separate codegen recipe.
"""
from __future__ import annotations
import json
from pathlib import Path
import tomllib

HOT = {'migo-audio', 'migo-io', 'migo-services', 'migo-shared', 'migo-alloc-probe',
       'migo-executor-probe', 'rubato', 'audioadapter', 'audioadapter-buffers',
       'audioadapter-sample', 'tokio', 'minimp3-sys', 'symphonia-core',
       'symphonia-codec-pcm', 'symphonia-format-riff', 'symphonia-format-ogg',
       'symphonia-codec-vorbis'}


def inputs(root: Path):
    manifest = root / 'engine/Cargo.toml'
    data = tomllib.loads(manifest.read_text())
    members = []
    for member in data['workspace']['members']:
        assert not any(c in member for c in '*?['), 'explicit member manifests required'
        path = root / 'engine' / member / 'Cargo.toml'
        assert path.is_file(), path
        members.append(path)
    return [Path(__file__).resolve(), manifest, *members]


def metadata(log: Path):
    rows = []
    for line in log.read_text().splitlines():
        try: row = json.loads(line)
        except ValueError: continue
        if isinstance(row, dict) and 'packages' in row and 'workspace_members' in row:
            rows.append(row)
    assert len(rows) == 1, log
    return rows[0]


def apply(root, work, package, env, common, policy='root'):
    assert policy in ['root', 'unoptimized']
    root_log, selected_log = work / 'root-workspace-metadata.jsonl', work / 'profile-metadata.jsonl'
    assert common.run(['cargo', 'metadata', '--offline', '--locked', '--no-deps', '--format-version', '1'],
        root_log, root / 'engine', env) == 0
    assert common.run(['cargo', 'metadata', '--offline', '--locked', '--format-version', '1'],
        selected_log, package, env) == 0
    original, selected = metadata(root_log), metadata(selected_log)
    ids = set(original['workspace_members'])
    workspace = {p['name'] for p in original['packages'] if p['id'] in ids}
    selected_names = {p['name'] for p in selected['packages']}
    root_toml = tomllib.loads((root / 'engine/Cargo.toml').read_text())
    profiles = root_toml.get('profile', {})
    assert 'test' not in profiles, 'explicit test policy needs its own profile mapping'
    dev = profiles['dev']; base = {k:v for k,v in dev.items() if k not in ['package', 'build-override']}
    assert set(dev.get('package', {}).get('*', {})) <= {'opt-level'}, 'new dependency knobs need explicit member defaults'
    packages = dev.get('package', {})
    fragment = ''
    if policy == 'root':
        def section(name, values):
            return '\n[' + name + ']\n' + ''.join(k + '=' + json.dumps(v) + '\n' for k,v in values.items())
        fragment += section('profile.dev', base)
        if 'build-override' in dev:
            fragment += section('profile.dev.build-override', dev['build-override'])
        for name, values in packages.items():
            if name == '*' or name in selected_names:
                fragment += section('profile.dev.package.' + json.dumps(name), values)
        for name in sorted(workspace & selected_names):
            if name not in packages:
                fragment += section('profile.dev.package.' + json.dumps(name), {'opt-level': base.get('opt-level', 0)})
        manifest = package / 'Cargo.toml'; text = manifest.read_text()
        assert '[profile.dev' not in text and '[profile.test' not in text
        manifest.write_text(text + fragment)
    expected = {}
    for name in sorted(HOT & selected_names):
        values = dict(base)
        if policy == 'root':
            if name not in workspace: values.update(packages.get('*', {}))
            values.update(packages.get(name, {}))
        else:
            values = {'opt-level': 0, 'debug': True}
        expected[name] = dict(opt_level=str(values.get('opt-level', 0)),
            debuginfo=2 if values.get('debug', True) is True else values['debug'],
            debug_assertions=values.get('debug-assertions', True), overflow_checks=values.get('overflow-checks', True))
    result = dict(policy=policy, root_workspace_names=sorted(workspace), selected_workspace_names=sorted(workspace & selected_names),
        expected=expected, generated_fragment=fragment, release_policy_changed=False,
        root_manifest_sha256=common.sha(root / 'engine/Cargo.toml'))
    (work / 'debug-profile-policy.json').write_text(json.dumps(result, indent=2) + '\n')
    assert common.run(['cargo', 'check', '--lib', '--offline', '--locked'], work / 'profile-library-check.log', package, env, timeout=300) == 0
    return result


def artifact_profiles(log):
    result = {}
    for line in log.read_text().splitlines():
        try: row = json.loads(line)
        except ValueError: continue
        if row.get('reason') != 'compiler-artifact' or row['target']['kind'] != ['lib']: continue
        identity = row['package_id'].rsplit('#', 1)[-1]
        name = identity.rsplit('@', 1)[0]
        if name not in HOT: continue
        profile = row['profile']
        value = {k:profile[k] for k in ['opt_level','debuginfo','debug_assertions','overflow_checks']}
        assert name not in result or result[name] == value, (name, 'ambiguous host/target profile')
        result[name] = value
    return result


def verify(log, policy, output):
    actual = artifact_profiles(log)
    expected = policy['expected']
    assert actual.keys() >= expected.keys(), ('missing debug artifacts', expected.keys() - actual.keys())
    mismatch = {n:dict(expected=v,actual=actual[n]) for n,v in expected.items() if actual[n] != v}
    output.write_text(json.dumps(dict(policy=policy['policy'], checked=actual, mismatches=mismatch), indent=2) + '\n')
    assert not mismatch, ('debug codegen diverged from recorded policy', mismatch)
    return actual
