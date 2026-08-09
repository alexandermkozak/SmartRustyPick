#!/usr/bin/env python3
"""Compare two `performance_metrics.json` files and report regressions.

The performance suites write every measurement to `performance_metrics.json`. Absolute
numbers are only comparable within one machine, so the intended use is: run the suites
on the base revision, keep the file, run them again on the change, and diff the two.

    make test-performance && cp performance_metrics.json /tmp/base.json
    git switch my-change && make test-performance
    python3 scripts/compare_perf.py /tmp/base.json performance_metrics.json

Exits non-zero when any metric regressed by more than the tolerance, so it can be used
as a gate in a pipeline that controls both runs.
"""

import argparse
import json
import sys

# Metrics whose value should be compared even though they are not latencies.
LOWER_IS_BETTER = ("p95_ms", "p50_ms", "p99_ms", "mean_ms", "ratio", "peak_rss_mb", "cpu_seconds")
HIGHER_IS_BETTER = ("ops_per_second", "aggregate_ops_per_second", "scaling")


def load(path):
    with open(path) as handle:
        return json.load(handle).get("metrics", {})


def field_of(payload):
    """Pick the single most representative number of a metric payload."""
    for key in LOWER_IS_BETTER:
        if key in payload and payload[key] is not None:
            return key, payload[key], True
    for key in HIGHER_IS_BETTER:
        if key in payload and payload[key] is not None:
            return key, payload[key], False
    return None, None, True


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("baseline")
    parser.add_argument("candidate")
    parser.add_argument(
        "--tolerance",
        type=float,
        default=25.0,
        help="percentage a metric may worsen before it counts as a regression (default: 25)",
    )
    args = parser.parse_args()

    baseline = load(args.baseline)
    candidate = load(args.candidate)

    rows = []
    regressions = []
    for name in sorted(set(baseline) & set(candidate)):
        key, new_value, lower_is_better = field_of(candidate[name])
        old_value = baseline[name].get(key) if key else None
        if key is None or old_value in (None, 0):
            continue
        change = (new_value - old_value) / old_value * 100.0
        worse = change if lower_is_better else -change
        verdict = "regression" if worse > args.tolerance else ("better" if worse < -args.tolerance else "same")
        if verdict == "regression":
            regressions.append(name)
        rows.append((name, key, old_value, new_value, change, verdict))

    only_new = sorted(set(candidate) - set(baseline))
    only_old = sorted(set(baseline) - set(candidate))

    print("| Metric | Field | Baseline | Candidate | Change | Verdict |")
    print("| --- | --- | --- | --- | --- | --- |")
    for name, key, old_value, new_value, change, verdict in rows:
        print(f"| {name} | {key} | {old_value:g} | {new_value:g} | {change:+.1f}% | {verdict} |")

    if only_new:
        print(f"\nNew metrics (no baseline): {', '.join(only_new)}")
    if only_old:
        print(f"\nMissing from the candidate run: {', '.join(only_old)}")

    if regressions:
        print(
            f"\n{len(regressions)} metric(s) regressed by more than {args.tolerance:g}%: "
            f"{', '.join(regressions)}",
            file=sys.stderr,
        )
        return 1
    print(f"\nNo metric regressed by more than {args.tolerance:g}%.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
