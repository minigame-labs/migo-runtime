#!/usr/bin/env python3
"""
Validate the device test suite report JSON against CI quality gates.

Usage:
    python scripts/ci/check_migo_test_suite.py \\
        --report ../<device-test-suite>/report.json \\
        [--min-pass-rate 95] \\
        [--min-total 100] \\
        [--required-categories canvas,webgl,audio,touch,network] \\
        [--summary-out reports/test-suite-summary.json] \\
        [--gate]

Expected report.json format (from the device test suite):
    {
      "summary": {
        "total": 500,
        "passed": 485,
        "failed": 10,
        "skipped": 5
      },
      "categories": {
        "canvas": { "total": 50, "passed": 48, "failed": 2, "skipped": 0 },
        "webgl":  { "total": 80, "passed": 78, "failed": 1, "skipped": 1 },
        ...
      },
      "failures": [
        { "name": "test_name", "category": "canvas", "error": "..." },
        ...
      ]
    }

Exit codes:
    0 - all gates pass
    1 - one or more gates fail (only in --gate mode)
    2 - usage / file error
"""

import argparse
import json
import os
import sys
from datetime import datetime, timezone


DEFAULT_MIN_PASS_RATE = 95.0
DEFAULT_MIN_TOTAL = 100
DEFAULT_REQUIRED_CATEGORIES = [
    "canvas", "webgl", "audio", "touch", "network",
    "storage", "timer", "lifecycle",
]
REPORT_BINDING_FIELDS = (
    "_source_revision",
    "_artifact_sha256",
    "_installed_native_sha256",
    "_device_abi",
    "_profile",
    "_package",
)


def load_json(path):
    """Load and parse a JSON file."""
    try:
        with open(path, "r") as f:
            return json.load(f)
    except FileNotFoundError:
        print(f"ERROR: report not found: {path}", file=sys.stderr)
        sys.exit(2)
    except json.JSONDecodeError as e:
        print(f"ERROR: invalid JSON in {path}: {e}", file=sys.stderr)
        sys.exit(2)


def validate_report_structure(report):
    """Check that the report has the expected structure."""
    errors = []
    if "summary" not in report:
        errors.append("missing 'summary' field")
    else:
        for field in ("total", "passed"):
            if field not in report["summary"]:
                errors.append(f"missing 'summary.{field}'")
    if "categories" not in report:
        errors.append("missing 'categories' field")
    return errors


def validate_report_bindings(report, bindings):
    """Require the external suite report to name the release it exercised."""
    for field, expected in bindings.items():
        if report.get(field) != expected:
            raise ValueError(f"test-suite report binding mismatch: {field}")


def check_pass_rate(summary, min_rate):
    """Check overall pass rate."""
    total = summary.get("total", 0)
    passed = summary.get("passed", 0)

    if total == 0:
        return {
            "gate": "pass_rate",
            "status": "fail",
            "message": "no tests executed (total=0)",
            "actual": 0,
            "threshold": min_rate,
        }

    rate = passed / total * 100
    status = "pass" if rate >= min_rate else "fail"
    return {
        "gate": "pass_rate",
        "status": status,
        "message": f"pass rate {rate:.1f}% ({passed}/{total})"
                   + (f" >= {min_rate}%" if status == "pass" else f" < {min_rate}%"),
        "actual": round(rate, 1),
        "threshold": min_rate,
    }


def check_min_total(summary, min_total):
    """Check minimum test count."""
    total = summary.get("total", 0)
    status = "pass" if total >= min_total else "fail"
    return {
        "gate": "min_total",
        "status": status,
        "message": f"total tests {total}"
                   + (f" >= {min_total}" if status == "pass" else f" < {min_total}"),
        "actual": total,
        "threshold": min_total,
    }


def check_required_categories(categories, required):
    """Check that all required categories are present and have tests."""
    results = []
    for cat in required:
        if cat not in categories:
            results.append({
                "gate": f"category:{cat}",
                "status": "fail",
                "message": f"required category '{cat}' not found in report",
                "actual": None,
            })
        elif categories[cat].get("total", 0) == 0:
            results.append({
                "gate": f"category:{cat}",
                "status": "fail",
                "message": f"required category '{cat}' has 0 tests",
                "actual": 0,
            })
        else:
            cat_total = categories[cat]["total"]
            cat_passed = categories[cat].get("passed", 0)
            cat_rate = cat_passed / cat_total * 100 if cat_total > 0 else 0
            results.append({
                "gate": f"category:{cat}",
                "status": "pass",
                "message": f"category '{cat}': {cat_passed}/{cat_total} ({cat_rate:.0f}%)",
                "actual": cat_total,
            })
    return results


def print_report(report, gate_results, has_failures):
    """Print human-readable report."""
    summary = report.get("summary", {})
    categories = report.get("categories", {})
    failures = report.get("failures", [])

    print("=" * 70)
    print("  Migo Test Suite Report")
    print("=" * 70)

    # Overall summary
    total = summary.get("total", 0)
    passed = summary.get("passed", 0)
    failed = summary.get("failed", 0)
    skipped = summary.get("skipped", 0)
    rate = passed / total * 100 if total > 0 else 0

    print(f"\n  Overall: {passed}/{total} passed ({rate:.1f}%)")
    print(f"  Failed: {failed}  |  Skipped: {skipped}")

    # Category breakdown
    if categories:
        print(f"\n  {'Category':<20} {'Passed':>8} {'Total':>8} {'Rate':>8}")
        print(f"  {'-' * 48}")
        for cat_name in sorted(categories.keys()):
            cat = categories[cat_name]
            ct = cat.get("total", 0)
            cp = cat.get("passed", 0)
            cr = cp / ct * 100 if ct > 0 else 0
            flag = " *" if cr < 90 else ""
            print(f"  {cat_name:<20} {cp:>8} {ct:>8} {cr:>7.0f}%{flag}")

    # Gate results
    print(f"\n  {'─' * 50}")
    print(f"  Quality Gates:")
    status_icon = {"pass": "  OK ", "fail": "FAIL "}
    for gr in gate_results:
        icon = status_icon.get(gr["status"], "???? ")
        print(f"    [{icon}] {gr['message']}")

    # Top failures
    if failures:
        print(f"\n  Top Failures (up to 20):")
        for f in failures[:20]:
            name = f.get("name", "?")
            cat = f.get("category", "?")
            err = f.get("error", "")
            # Truncate long error messages
            if len(err) > 80:
                err = err[:77] + "..."
            print(f"    [{cat}] {name}: {err}")
        if len(failures) > 20:
            print(f"    ... and {len(failures) - 20} more")

    print()
    print("-" * 70)
    if has_failures:
        print("  RESULT: FAIL")
    else:
        print("  RESULT: PASS")
    print("=" * 70)


def write_summary(path, report, gate_results, has_failures, bindings):
    """Write machine-readable summary."""
    summary_out = {
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "overall": "fail" if has_failures else "pass",
        "summary": report.get("summary", {}),
        "gates": gate_results,
        "failure_count": len(report.get("failures", [])),
    }
    summary_out.update(bindings)

    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        json.dump(summary_out, f, indent=2)
    print(f"\n  Summary written to: {path}")


def main():
    parser = argparse.ArgumentParser(
        description="Validate device test suite results against CI gates."
    )
    parser.add_argument(
        "--report", required=True,
        help="Path to the device test suite report JSON",
    )
    parser.add_argument(
        "--min-pass-rate", type=float, default=DEFAULT_MIN_PASS_RATE,
        help=f"Minimum overall pass rate %% (default: {DEFAULT_MIN_PASS_RATE})",
    )
    parser.add_argument(
        "--min-total", type=int, default=DEFAULT_MIN_TOTAL,
        help=f"Minimum total test count (default: {DEFAULT_MIN_TOTAL})",
    )
    parser.add_argument(
        "--required-categories", type=str, default=None,
        help="Comma-separated list of required categories "
             f"(default: {','.join(DEFAULT_REQUIRED_CATEGORIES)})",
    )
    parser.add_argument(
        "--summary-out", default=None,
        help="Path to write summary JSON",
    )
    parser.add_argument(
        "--gate", action="store_true",
        help="Enable CI gating (exit 1 on failure)",
    )
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--artifact-sha256", required=True)
    parser.add_argument("--installed-native-sha256", required=True)
    parser.add_argument("--device-abi", required=True)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--package", required=True)

    args = parser.parse_args()

    required_cats = (
        args.required_categories.split(",")
        if args.required_categories
        else DEFAULT_REQUIRED_CATEGORIES
    )

    report = load_json(args.report)
    bindings = {
        "_source_revision": args.source_revision,
        "_artifact_sha256": args.artifact_sha256,
        "_installed_native_sha256": args.installed_native_sha256,
        "_device_abi": args.device_abi,
        "_profile": args.profile,
        "_package": args.package,
    }

    try:
        validate_report_bindings(report, bindings)
    except ValueError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        sys.exit(2)

    # Validate structure
    struct_errors = validate_report_structure(report)
    if struct_errors:
        print("ERROR: invalid report structure:", file=sys.stderr)
        for e in struct_errors:
            print(f"  - {e}", file=sys.stderr)
        sys.exit(2)

    # Run gates
    summary = report["summary"]
    categories = report.get("categories", {})

    gate_results = []
    gate_results.append(check_pass_rate(summary, args.min_pass_rate))
    gate_results.append(check_min_total(summary, args.min_total))
    gate_results.extend(check_required_categories(categories, required_cats))

    has_failures = any(g["status"] == "fail" for g in gate_results)

    print_report(report, gate_results, has_failures)

    if args.summary_out:
        write_summary(args.summary_out, report, gate_results, has_failures, bindings)

    if has_failures and args.gate:
        sys.exit(1)
    sys.exit(0)


if __name__ == "__main__":
    main()
