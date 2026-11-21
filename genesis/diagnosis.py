import difflib
import json

from .isolation import ProviderUnavailable


REQUIRED = {"id", "lineage_parent", "failure_ids", "evidence", "suspected_files", "root_cause", "proposed_change", "expected_effect", "verification", "risk", "not_duplicate_of"}


def extract(text):
    decoder = json.JSONDecoder()
    for idx, char in enumerate(text):
        if char not in "[{":
            continue
        try:
            value, _ = decoder.raw_decode(text[idx:])
        except ValueError:
            continue
        items = value if isinstance(value, list) else value.get("hypotheses", [value]) if isinstance(value, dict) else []
        if items and all(isinstance(h, dict) and REQUIRED <= h.keys() for h in items):
            for h in items:
                if not all(isinstance(h[k], list) for k in ("failure_ids", "evidence", "suspected_files", "verification", "not_duplicate_of")):
                    raise ValueError("hypothesis list fields invalid")
                if not isinstance(h["proposed_change"], str) or not h["suspected_files"]:
                    raise ValueError("hypothesis change/files missing")
            return items
    raise ValueError("agent output contains no valid Hypothesis JSON")


def similar(hypothesis, failures, threshold):
    def signature(h):
        return " ".join(sorted(h.get("suspected_files", []))) + " " + h.get("proposed_change", "")
    return any(difflib.SequenceMatcher(None, signature(hypothesis), signature(f["hypothesis"])).ratio() > threshold for f in failures)


def diagnose(adapter, parent, worktree, failures, evidence, config):
    prompt = ("MODE: diagnose\nReturn exactly two distinct hypotheses as JSON array. Each must contain "
              + ", ".join(sorted(REQUIRED)) + ". Hypothesis B must address a different failure cluster or implementation.\n"
              + json.dumps({"parent": parent["agent_id"], "failures": evidence, "negative_experience": failures}, ensure_ascii=False))
    errors = []
    for attempt in range(min(2, config["diagnosis"]["retries"]) + 1):
        try:
            output = adapter.call(parent["binary"], prompt + "\nPrior errors: " + "; ".join(errors), worktree, "diagnose")
            hypotheses = extract(output)
            if len(hypotheses) != 2:
                raise ValueError("exactly two hypotheses required")
            if any(h["lineage_parent"] != parent["agent_id"] for h in hypotheses):
                raise ValueError("wrong lineage_parent")
            if any(similar(h, failures, config["diagnosis"]["similarity_threshold"]) for h in hypotheses):
                raise ValueError("duplicate failed hypothesis")
            return hypotheses, errors
        except ProviderUnavailable:
            raise
        except (ValueError, RuntimeError, TimeoutError) as exc:
            errors.append(str(exc))
    return [], errors
