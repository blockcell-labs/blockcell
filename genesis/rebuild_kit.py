"""Rebuild a clean EAB kit from archived evidence, verifying original hashes."""
import argparse
import hashlib
import json
import shutil
from pathlib import Path


def rebuild(archive, destination):
    archive, destination = Path(archive), Path(destination).expanduser()
    cases = []
    fields = ('id', 'module', 'module_name', 'title', 'severity', 'oracle', 'expected', 'acceptance_criteria')
    evidence = sorted((archive / 'evidence').glob('*/case.json'))
    for path in evidence:
        old = json.loads(path.read_text())
        row = {key: old[key] for key in fields}
        row['prompt'] = old.get('kit_prompt') or old['prompts'][0]
        cases.append(row)
    if len(cases) != 60 or len({c['id'] for c in cases}) != 60:
        raise ValueError('expected exactly 60 unique cases')
    initial = json.loads((archive / 'evidence/A01/workspace_before.json').read_text())
    fixtures = {}
    for name, metadata in initial.items():
        if '/' in name or name.startswith('.') or name.endswith(('.db', '.db-shm', '.db-wal')):
            continue
        source = archive / 'runtime/workspace' / name
        digest = hashlib.sha256(source.read_bytes()).hexdigest()
        if digest != metadata['sha256']:
            raise ValueError('fixture changed from original snapshot: ' + name)
        fixtures[name] = digest
    workspace = destination / 'fixtures/workspace'
    workspace.mkdir(parents=True, exist_ok=True)
    if any(p.name not in fixtures for p in workspace.iterdir()):
        raise ValueError('destination contains non-fixture files')
    for name in fixtures:
        shutil.copy2(archive / 'runtime/workspace' / name, workspace / name)
    (destination / 'cases').mkdir(exist_ok=True)
    (destination / 'cases/blockcell_eab60.jsonl').write_text(''.join(json.dumps(c, ensure_ascii=False) + '\n' for c in cases))
    (destination / 'provenance.json').write_text(json.dumps({'source': str(archive), 'case_count': len(cases), 'fixture_hashes': fixtures}, indent=2))
    return {'cases': len(cases), 'fixtures': len(fixtures), 'destination': str(destination)}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('archive')
    parser.add_argument('destination')
    args = parser.parse_args()
    print(json.dumps(rebuild(args.archive, args.destination)))
