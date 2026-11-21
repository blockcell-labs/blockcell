import random


class Selector:
    def __init__(self, config, rng=None):
        self.config = config
        self.rng = rng or random.Random()

    def select(self, agents, force_archive=False):
        pool = [a for a in agents if a["status"] in ("PARENT", "ARCHIVE")]
        if not self.config['promotion'].get('commit_enabled', True):
            pool = [a for a in pool if a['status'] == 'PARENT']
        if force_archive:
            archives = [a for a in pool if a["status"] == "ARCHIVE"]
            if archives:
                pool = archives
        elif self.rng.random() < self.config["selector"]["exploit_probability"]:
            parents = sorted((a for a in pool if a["status"] == "PARENT"), key=lambda a: a["pass_count"], reverse=True)
            pool = parents[:self.config["selector"]["top_k"]] or pool
        if not pool:
            raise RuntimeError("no eligible parent")
        weights = [1 / ((1 + a["children_count"]) * (1 + a["no_promote_rounds"])) for a in pool]
        return self.rng.choices(pool, weights=weights, k=1)[0]
