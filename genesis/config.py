import hashlib
import copy
import json
import re
from pathlib import Path


def expand(template, **values):
    return re.sub(r"\{([a-z_]+)\}", lambda match: str(values[match[1]]) if match[1] in values else match[0], template)


def load(path=None):
    path = Path(path or Path(__file__).with_name("genesis.json"))
    config = json.loads(path.read_text())
    evaluation_config = copy.deepcopy(config)
    evaluation_config['promotion'].pop('commit_enabled', None)
    config["_hash"] = hashlib.sha256(json.dumps(evaluation_config, sort_keys=True).encode()).hexdigest()
    config["_root"] = str(Path(config["genesis"]["workspace_root"]).resolve())
    if config["genesis"]["candidates_per_cycle"] != 2:
        raise ValueError("Genesis v0.1 requires two candidates per cycle")
    eab = config["benchmarks"]["L3"]["eab60"]
    if set(eab["dev"]) & set(eab["holdout"]):
        raise ValueError("EAB60 dev and holdout must be disjoint")
    return config
