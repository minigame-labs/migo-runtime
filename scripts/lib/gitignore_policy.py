"""Check repository ignore rules without reading or changing its Git index.

Use an isolated temporary Git repository with the same applicable ignore files.
Callers supply published/declared source inputs, never infer their absence from
the set of files surviving a checkout.
"""
from pathlib import Path
import os
import subprocess
import tempfile


def matching_rules(root: Path, paths) -> dict[str, tuple[str, str]]:
    paths = sorted(set(paths))
    ignores = {Path('.gitignore')}
    for name in paths:
        path = Path(name)
        if path.is_absolute() or '..' in path.parts:
            raise ValueError('ignore contract paths must be repository-relative')
        ignores.update(parent / '.gitignore' for parent in path.parents)
    environment = {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'LANG': 'C.UTF-8',
                   'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': os.devnull,
                   'GIT_TERMINAL_PROMPT': '0'}
    with tempfile.TemporaryDirectory(prefix='migo-ignore-policy-') as temporary:
        scratch = Path(temporary)
        subprocess.run(['git', 'init', '--quiet', '--template=', str(scratch)],
                       env=environment, check=True, capture_output=True)
        for name in ignores:
            source = root / name
            if source.is_file():
                target = scratch / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(source.read_bytes())
        result = subprocess.run(['git', '-C', str(scratch), '-c', 'core.excludesFile='+os.devnull,
                                 'check-ignore', '--no-index', '--verbose', '-z', '--stdin'],
                                input=b''.join(name.encode()+b'\0' for name in paths),
                                env=environment, capture_output=True)
        if result.returncode not in (0, 1):
            raise RuntimeError(result.stderr.decode(errors='replace'))
        fields = result.stdout.decode(errors='surrogateescape').split('\0')
        fields = fields[:-1] if fields[-1:] == [''] else fields
        if len(fields) % 4:
            raise RuntimeError('unexpected git check-ignore output')
        return {fields[i+3]: (fields[i], fields[i+2]) for i in range(0, len(fields), 4)}


def check(root: Path, required=(), generated=()) -> None:
    required, generated = list(required), list(generated)
    rules = matching_rules(root, required + generated)
    errors = []
    for name in required:
        source, pattern = rules.get(name, ('', ''))
        if pattern and not pattern.startswith('!'):
            errors.append(f'required source ignored: {name} ({source}: {pattern})')
    for name in generated:
        source, pattern = rules.get(name, ('', ''))
        if not pattern or pattern.startswith('!'):
            errors.append(f'generated output not ignored: {name}')
    if errors:
        shown = '\n'.join(errors[:30])
        raise ValueError(f'Git ignore policy: {len(errors)} violations\n{shown}')


def tracked_ignored(root: Path) -> list[str]:
    """Catch ignored files already in the index, including old commits.

    Read paths only: never refresh, lock or rewrite the caller's index. Use the
    repository's declared rules, independent of a maintainer's global excludes.
    """
    result = subprocess.run(['git', '--no-optional-locks', '-C', str(root),
                             'ls-files', '-z', '--cached'],
                            check=True, capture_output=True)
    paths = [p.decode(errors='surrogateescape') for p in result.stdout.split(b'\0') if p]
    rules = matching_rules(root, paths)
    return sorted(name for name, (_, pattern) in rules.items()
                  if pattern and not pattern.startswith('!'))
