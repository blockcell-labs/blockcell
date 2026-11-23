import hashlib
import copy
import json
import os
import re
from pathlib import Path


def expand(template, **values):
    return re.sub(r"\{([a-z_]+)\}", lambda match: str(values[match[1]]) if match[1] in values else match[0], template)


def evaluation_hash(config):
    settings = config['agent']
    evaluation_config = {
        'agent': {key: settings.get(key) for key in ('command', 'model', 'provider', 'upstream', 'timeout', 'concurrency', 'require_bwrap')},
        'dataset_version': config['benchmarks']['dataset_version'],
        'L3': copy.deepcopy(config['benchmarks']['L3']),
    }
    evaluation_config['agent']['upstream_stream'] = settings.get('upstream_stream', True)
    kit = evaluation_config['L3']['eab60'].get('kit') or os.environ.get('EAB60_KIT')
    if kit:
        root = Path(kit).resolve()
        files = {}
        for path in sorted(root.rglob('*')):
            relative = path.relative_to(root)
            if path.is_file() and not {'.git', '__pycache__'}.intersection(relative.parts):
                with path.open('rb') as stream:
                    digest = hashlib.sha256()
                    for block in iter(lambda: stream.read(1024 * 1024), b''):
                        digest.update(block)
                    files[relative.as_posix()] = digest.hexdigest()
        evaluation_config['eab60_kit'] = {'path': str(root), 'files': files}
    for options in evaluation_config['L3'].values():
        for key in ('required', 'safety_ids'):
            options.pop(key, None)
    return hashlib.sha256(json.dumps(evaluation_config, sort_keys=True).encode()).hexdigest()


def load(path=None):
    path = Path(path or Path(__file__).with_name("genesis.json"))
    config = json.loads(path.read_text())
    for variable, field in (('OPENAI_BASE_URL', 'upstream'), ('OPENAI_MODEL', 'model')):
        if os.environ.get(variable):
            config['agent'][field] = os.environ[variable]
    concurrency = config['agent'].get('concurrency', 1)
    if type(config['agent'].get('upstream_stream', True)) is not bool:
        raise ValueError('upstream_stream must be boolean')
    if type(concurrency) is not int or not 1 <= concurrency <= 5:
        raise ValueError('concurrency must be an integer from 1 to 5')
    config['_hash'] = evaluation_hash(config)
    config["_root"] = str(Path(config["genesis"]["workspace_root"]).resolve())
    if config["genesis"]["candidates_per_cycle"] != 2:
        raise ValueError("Genesis v0.1 requires two candidates per cycle")
    eab = config["benchmarks"]["L3"]["eab60"]
    if set(eab["dev"]) & set(eab["holdout"]):
        raise ValueError("EAB60 dev and holdout must be disjoint")
    return config
