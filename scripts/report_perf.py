#!/usr/bin/env python3
"""Render `performance_metrics.json` as a Markdown report.

The performance suites already write every measurement to
`target/test-results/performance_metrics.json`. That file is machine-readable but
unpleasant to read, and in CI it only exists inside a build artifact. This turns it
into the table that belongs in a job summary or a pull request comment, so the
numbers of a run are visible without opening the workflow at all.

    make test-performance
    python3 scripts/report_perf.py > report.md

Absolute timings are only comparable within one machine, so the report states the
host and the knobs the run used, and leans on the budget and ratio checks - which
are calibrated per host and host-independent respectively - for its verdicts. To
diff two runs of the same machine, use `scripts/compare_perf.py` instead.
"""

import argparse
import json
import os
import sys

# Payload keys that identify what kind of measurement a metric is. Each metric is
# rendered by the first shape it matches, so the order here is the priority order.
LATENCY_KEY = "p95_ms"
RATIO_KEY = "ratio"
RESOURCE_KEY = "peak_rss_mb"
THROUGHPUT_KEY = "aggregate_ops_per_second"


def split_suite(name):
    """`"Performance: Full scan"` -> `("Performance", "Full scan")`."""
    suite, separator, measurement = name.partition(": ")
    return (suite, measurement) if separator else ("Other", name)


def number(value, digits=2):
    if value is None:
        return "-"
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return str(value)
    if digits == 0:
        return f"{value:,.0f}"
    return f"{value:.{digits}f}".rstrip("0").rstrip(".")


def table(header, rows):
    if not rows:
        return []
    lines = ["| " + " | ".join(header) + " |", "| " + " | ".join("---" for _ in header) + " |"]
    lines += ["| " + " | ".join(row) + " |" for row in rows]
    return lines + [""]


def budget_cell(payload):
    """Verdict for a latency metric against its (already host-scaled) p95 budget."""
    budget = payload.get("budget_p95_ms")
    if budget is None:
        return "-"
    p95 = payload.get(LATENCY_KEY)
    if p95 is None:
        return f"<= {number(budget)}"
    verdict = "ok" if p95 <= budget else "**EXCEEDED**"
    return f"{verdict} (<= {number(budget)})"


def classify(metrics):
    """Bucket every metric by the shape of its payload."""
    buckets = {"latency": [], "ratio": [], "resource": [], "throughput": [], "other": []}
    for name in sorted(metrics):
        payload = metrics[name]
        if not isinstance(payload, dict):
            continue
        if LATENCY_KEY in payload:
            kind = "latency"
        elif RATIO_KEY in payload:
            kind = "ratio"
        elif RESOURCE_KEY in payload:
            kind = "resource"
        elif THROUGHPUT_KEY in payload:
            kind = "throughput"
        else:
            kind = "other"
        buckets[kind].append((name, payload))
    return buckets


def latency_rows(entries):
    rows = []
    for name, payload in entries:
        suite, measurement = split_suite(name)
        rows.append(
            [
                f"{suite}: {measurement}",
                number(payload.get("p50_ms"), 3),
                number(payload.get(LATENCY_KEY), 3),
                number(payload.get("p99_ms"), 3),
                number(payload.get("max_ms"), 3),
                number(payload.get("ops_per_second"), 0),
                str(payload.get("count", "-")),
                budget_cell(payload),
            ]
        )
    return rows


def ratio_rows(entries):
    rows = []
    for name, payload in entries:
        suite, measurement = split_suite(name)
        ratio, limit = payload.get(RATIO_KEY), payload.get("limit")
        verdict = "-"
        if ratio is not None and limit:
            verdict = "ok" if ratio <= limit else "**EXCEEDED**"
            headroom = f" ({(1 - ratio / limit) * 100:.0f}% headroom)" if ratio <= limit else ""
            verdict += headroom
        rows.append([f"{suite}: {measurement}", f"{number(ratio, 3)}x", f"{number(limit, 2)}x", verdict])
    return rows


def resource_rows(entries):
    rows = []
    for name, payload in entries:
        suite, _ = split_suite(name)
        rows.append(
            [
                suite,
                number(payload.get("peak_rss_mb")),
                number(payload.get("final_rss_mb")),
                number(payload.get("cpu_seconds")),
                number(payload.get("wall_seconds")),
            ]
        )
    return rows


def other_rows(entries):
    """Anything without a known shape: print its fields so nothing is silently dropped."""
    rows = []
    for name, payload in entries:
        suite, measurement = split_suite(name)
        fields = ", ".join(f"{key} {number(value, 3)}" for key, value in sorted(payload.items()))
        rows.append([f"{suite}: {measurement}", fields])
    return rows


def throughput_rows(entries):
    rows = []
    for name, payload in entries:
        suite, measurement = split_suite(name)
        scaling, minimum = payload.get("scaling"), payload.get("minimum")
        verdict = "-"
        if scaling is not None and minimum is not None:
            verdict = "ok" if scaling >= minimum else "**BELOW MINIMUM**"
            verdict += f" (>= {number(minimum, 2)}x)"
        rows.append(
            [
                f"{suite}: {measurement}",
                str(payload.get("clients", "-")),
                number(payload.get("single_ops_per_second"), 0),
                number(payload.get(THROUGHPUT_KEY), 0),
                f"{number(scaling, 2)}x",
                verdict,
            ]
        )
    return rows


def environment_lines():
    """The knobs that decide what the numbers mean, so a table is never read out of context."""
    knobs = [
        ("Records", os.environ.get("SRP_PERF_RECORDS", "10000")),
        ("Concurrent clients", os.environ.get("SRP_CONC_CLIENTS", "8")),
        ("Budget scale", os.environ.get("SRP_PERF_BUDGET_SCALE", "1")),
    ]
    runner = os.environ.get("RUNNER_OS")
    if runner:
        knobs.append(("Runner", f"{runner} / {os.environ.get('RUNNER_ARCH', 'unknown')}"))
    return ", ".join(f"{label} {value}" for label, value in knobs)


def render(document, title, commit=None, footer=None):
    metrics = document.get("metrics", {})
    buckets = classify(metrics)

    lines = [f"## {title}", ""]
    context = [f"{len(metrics)} measurements", environment_lines()]
    if commit:
        context.insert(0, f"commit `{commit[:12]}`")
    generated = document.get("generated")
    if generated:
        context.append(f"generated {generated}")
    lines += [" - ".join(part for part in context if part), ""]
    lines += [
        "Absolute timings are comparable only within one machine; shared runners are noisy.",
        "The budget and ratio verdicts below are the load-bearing signals.",
        "",
    ]

    lines += ["### Latency", ""]
    lines += table(
        ["Measurement", "p50 ms", "p95 ms", "p99 ms", "max ms", "ops/s", "n", "p95 budget"],
        latency_rows(buckets["latency"]),
    ) or ["No latency measurements recorded.", ""]

    if buckets["ratio"]:
        lines += ["### Complexity and fairness ratios", ""]
        lines += [
            "Host-independent: they measure how cost grows, not how fast the host is.",
            "",
        ]
        lines += table(["Check", "Ratio", "Limit", "Verdict"], ratio_rows(buckets["ratio"]))

    if buckets["throughput"]:
        lines += ["### Throughput scaling", ""]
        lines += table(
            ["Check", "Clients", "Single ops/s", "Aggregate ops/s", "Scaling", "Verdict"],
            throughput_rows(buckets["throughput"]),
        )

    if buckets["resource"]:
        lines += ["### Server resources", ""]
        lines += table(
            ["Suite", "Peak RSS MB", "Final RSS MB", "CPU s", "Wall s"],
            resource_rows(buckets["resource"]),
        )

    if buckets["other"]:
        lines += ["### Other measurements", ""]
        lines += table(["Measurement", "Values"], other_rows(buckets["other"]))

    if footer:
        lines += ["", footer]

    return "\n".join(lines).rstrip() + "\n"


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    results_dir = os.environ.get("SRP_RESULTS_DIR", os.path.join("target", "test-results"))
    parser.add_argument(
        "metrics",
        nargs="?",
        default=os.path.join(results_dir, "performance_metrics.json"),
        help="performance_metrics.json to render (default: %(default)s)",
    )
    parser.add_argument("--title", default="Performance benchmark results")
    parser.add_argument("--commit", default=os.environ.get("GITHUB_SHA"))
    parser.add_argument("--footer", help="line appended at the end, e.g. a link to the run")
    parser.add_argument("-o", "--output", help="write here instead of stdout")
    args = parser.parse_args()

    try:
        with open(args.metrics) as handle:
            document = json.load(handle)
    except (OSError, ValueError) as exc:
        print(f"Cannot read {args.metrics}: {exc}", file=sys.stderr)
        return 1

    report = render(document, args.title, args.commit, args.footer)
    if args.output:
        with open(args.output, "w") as handle:
            handle.write(report)
    else:
        sys.stdout.write(report)
    return 0


if __name__ == "__main__":
    sys.exit(main())
