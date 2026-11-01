#!/usr/bin/env python3
"""Retired producer ops retain wire numbers without inventing unused engine imports.

The event-based audio facade retired get_state, leaving two contracts at odds:
classification required an import, while append-only service IDs required a lane.
Exercise the actual generator's retirement checks, preserving ID 136 and refusing
unknown, duplicate, active or malformed retirement records.
"""
from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def main():
    # Isolated generator mutants still use the real, unchanged source readers.
    sys.path.insert(0, str(ROOT/'scripts/lib'))
    path = Path(os.environ.get('MIGO_RETIREMENT_GENERATOR', ROOT/'scripts/gen-performance-plus-engine.py'))
    spec = importlib.util.spec_from_file_location('retirement_generator', path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    identity = json.loads((ROOT/module.SERVICE_OPS_CONTRACT).read_text())
    boundary = json.loads((ROOT/'contracts/runtime/op-boundary.json').read_text())
    lanes = {name: value if isinstance(value, str) else value['lane']
             for extension in boundary['extensions'].values() for name, value in extension['ops'].items()}
    lanes.update({module.wire_name(name): value
                  for name, value in boundary.get('core_members', {}).get('members', {}).items()})
    assert identity['ops']['op_inner_audio_get_state'] == 136
    assert identity['ops']['op_inner_audio_set_start_time'] == 292
    assert 'op_inner_audio_get_state' in identity['retired_ops']
    with tempfile.TemporaryDirectory(prefix='migo-service-retirement-') as directory:
        root = Path(directory)
        contract = root/module.SERVICE_OPS_CONTRACT
        producer = root/module.SERVICE_OPS_MODULE
        contract.parent.mkdir(parents=True)
        producer.parent.mkdir(parents=True)
        producer.write_bytes((ROOT/module.SERVICE_OPS_MODULE).read_bytes())
        contract.write_text(json.dumps(identity))
        try:
            assert module.check_service_ops(root, lanes) == identity['ops']
        except module.BuildError as error:
            raise AssertionError('valid retired service op was rejected') from error
        old = 'op_inner_audio_get_state'
        for retired in [[old, 'unknown_op'], [old]*2,
                        [old, 'op_inner_audio_play'], old, [old, None]]:
            invalid = dict(identity, retired_ops=retired)
            contract.write_text(json.dumps(invalid))
            try:
                module.check_service_ops(root, lanes)
            except module.BuildError:
                continue
            raise AssertionError(f'invalid retired service op accepted: {retired!r}')
    print('service retirement: retained IDs and 5 invalid records rejected by actual generator')


if __name__ == '__main__':
    main()
