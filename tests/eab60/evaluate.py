"""Extended evaluator for EAB-60 (replaces the kit's conservative evaluate.py).

The kit's `harness/evaluate.py` only tagged every case as "needs review" and
wrote a placeholder HTML page.  This evaluator is the extended version:

1. re-grades every stored evidence bundle with `graders.py` (deterministic
   checks over tool calls, session events, audit records, hook events and
   workspace diffs — no keyword-only scoring for numeric/artifact cases);
2. computes module scores (10 modules × 10 points, 0-4 per case);
3. writes `summary.json`, `review_queue.csv`, `capability_mapping.csv`,
   `report.html`.

Usage:
    python3 evaluate.py            # re-grade evidence + rebuild reports
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import regrade
import report


def main():
    print("== re-grade stored evidence ==")
    regrade.main()
    print("== build reports ==")
    report.main()


if __name__ == "__main__":
    main()
