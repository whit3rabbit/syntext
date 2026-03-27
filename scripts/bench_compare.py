#!/usr/bin/env python3
"""Benchmark ripline against rg and grep on an external Git repository.

This script is intentionally simple:
- It measures ripline index build time separately.
- It then reuses one built index to benchmark repeated searches.
- It compares against ripgrep and grep over the same repository.

Default grep mode uses `git ls-files` to avoid benchmarking recursive grep over
ignored/build output, which is usually a misleading baseline.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
DEFAULT_RIPLINE_BIN = REPO_ROOT / "target" / "release" / "ripline"
DEFAULT_PRESET_FILE = REPO_ROOT / "benchmarks" / "repo_presets.json"


@dataclass(frozen=True)
class QuerySpec:
    mode: str
    pattern: str

    @property
    def name(self) -> str:
        cleaned = self.pattern.replace("\n", " ").strip()
        if len(cleaned) > 48:
            cleaned = f"{cleaned[:45]}..."
        return f"{self.mode}:{cleaned}"


@dataclass(frozen=True)
class PresetSpec:
    name: str
    display_name: str
    repo_url: str
    suggested_local_path: str
    language_focus: str
    scale: str
    build_iterations: int
    search_iterations: int
    warmups: int
    tools: tuple[str, ...]
    queries: tuple[QuerySpec, ...]
    notes: tuple[str, ...]


def parse_query(value: str) -> QuerySpec:
    try:
        mode, pattern = value.split(":", 1)
    except ValueError as exc:
        raise argparse.ArgumentTypeError(
            f"invalid query {value!r}, expected literal:<pattern> or regex:<pattern>"
        ) from exc
    if mode not in {"literal", "regex"}:
        raise argparse.ArgumentTypeError(
            f"invalid query mode {mode!r}, expected literal or regex"
        )
    if not pattern:
        raise argparse.ArgumentTypeError("query pattern must not be empty")
    return QuerySpec(mode=mode, pattern=pattern)


def load_presets(path: Path) -> dict[str, PresetSpec]:
    raw = json.loads(path.read_text())
    presets: dict[str, PresetSpec] = {}
    for item in raw.get("presets", []):
        preset = PresetSpec(
            name=item["name"],
            display_name=item["display_name"],
            repo_url=item["repo_url"],
            suggested_local_path=item["suggested_local_path"],
            language_focus=item["language_focus"],
            scale=item["scale"],
            build_iterations=int(item["build_iterations"]),
            search_iterations=int(item["search_iterations"]),
            warmups=int(item["warmups"]),
            tools=tuple(item.get("tools", ["ripline", "rg", "grep"])),
            queries=tuple(parse_query(query) for query in item["queries"]),
            notes=tuple(item.get("notes", [])),
        )
        presets[preset.name] = preset
    return presets


def print_presets(presets: dict[str, PresetSpec]) -> None:
    print("# Benchmark Presets\n")
    for preset in sorted(presets.values(), key=lambda item: item.name):
        print(f"- `{preset.name}`: {preset.display_name}")
        print(f"  repo: `{preset.repo_url}`")
        print(f"  suggested local path: `{preset.suggested_local_path}`")
        print(f"  focus: `{preset.language_focus}`, scale: `{preset.scale}`")
        print(
            "  default settings: "
            f"build_iterations={preset.build_iterations}, "
            f"search_iterations={preset.search_iterations}, warmups={preset.warmups}"
        )
        print("  tools: " + ", ".join(f"`{tool}`" for tool in preset.tools))
        print(
            "  queries: "
            + ", ".join(f"`{query.name}`" for query in preset.queries)
        )
        if preset.notes:
            print("  notes: " + " ".join(preset.notes))
        print()


def tracked_files(repo_root: Path) -> bytes:
    result = subprocess.run(
        ["git", "-C", str(repo_root), "ls-files", "-z"],
        check=True,
        capture_output=True,
    )
    return result.stdout


def tracked_file_count(repo_root: Path) -> int:
    output = tracked_files(repo_root)
    if not output:
        return 0
    return sum(1 for part in output.split(b"\0") if part)


def ensure_ripline_binary(ripline_bin: Path) -> None:
    if ripline_bin.exists():
        return
    subprocess.run(
        ["cargo", "build", "--release", "--bin", "ripline"],
        cwd=REPO_ROOT,
        check=True,
    )


def run_timed(
    cmd: list[str] | str,
    *,
    cwd: Path,
    env: dict[str, str],
    shell: bool = False,
    allowed_codes: Iterable[int] = (0,),
) -> float:
    start = time.perf_counter()
    completed = subprocess.run(
        cmd,
        cwd=cwd,
        env=env,
        shell=shell,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        text=False,
    )
    elapsed_ms = (time.perf_counter() - start) * 1000.0
    if completed.returncode not in set(allowed_codes):
        raise RuntimeError(f"command failed with exit {completed.returncode}: {cmd!r}")
    return elapsed_ms


def output_line_count(
    cmd: list[str] | str,
    *,
    cwd: Path,
    env: dict[str, str],
    shell: bool = False,
    allowed_codes: Iterable[int] = (0,),
) -> int:
    completed = subprocess.run(
        cmd,
        cwd=cwd,
        env=env,
        shell=shell,
        capture_output=True,
        text=False,
    )
    if completed.returncode not in set(allowed_codes):
        raise RuntimeError(f"command failed with exit {completed.returncode}: {cmd!r}")
    stdout = completed.stdout
    if not stdout:
        return 0
    count = stdout.count(b"\n")
    if not stdout.endswith(b"\n"):
        count += 1
    return count


def summarize(samples_ms: list[float]) -> dict[str, float]:
    ordered = sorted(samples_ms)
    return {
        "median_ms": round(statistics.median(ordered), 3),
        "min_ms": round(ordered[0], 3),
        "max_ms": round(ordered[-1], 3),
    }


def ripline_search_cmd(
    ripline_bin: Path, repo_root: Path, index_dir: Path, query: QuerySpec
) -> list[str]:
    cmd = [
        str(ripline_bin),
        "--repo-root",
        str(repo_root),
        "--index-dir",
        str(index_dir),
        "search",
    ]
    if query.mode == "literal":
        cmd.append("--literal")
    cmd.append(query.pattern)
    return cmd


def rg_search_cmd(repo_root: Path, query: QuerySpec) -> list[str]:
    cmd = ["rg", "-n", "--no-heading", "--color", "never", "--hidden"]
    if query.mode == "literal":
        cmd.append("-F")
    cmd.extend([query.pattern, str(repo_root)])
    return cmd


def grep_search_cmd(
    repo_root: Path, tracked_list: Path, query: QuerySpec, grep_mode: str
) -> str:
    grep_flag = "-F" if query.mode == "literal" else "-E"
    pattern = shlex.quote(query.pattern)
    if grep_mode == "tracked":
        return (
            f"xargs -0 grep -nIH {grep_flag} -e {pattern} "
            f"< {shlex.quote(str(tracked_list))}"
        )
    return (
        f"grep -RInH --exclude-dir=.git {grep_flag} -e {pattern} "
        f"{shlex.quote(str(repo_root))}"
    )


def benchmark_command(
    cmd: list[str] | str,
    *,
    cwd: Path,
    env: dict[str, str],
    warmups: int,
    iterations: int,
    shell: bool = False,
    allowed_codes: Iterable[int] = (0, 1),
) -> dict[str, float]:
    for _ in range(warmups):
        run_timed(cmd, cwd=cwd, env=env, shell=shell, allowed_codes=allowed_codes)
    samples = [
        run_timed(cmd, cwd=cwd, env=env, shell=shell, allowed_codes=allowed_codes)
        for _ in range(iterations)
    ]
    return summarize(samples)


def parse_tools(value: str) -> tuple[str, ...]:
    allowed = {"ripline", "rg", "grep"}
    tools = tuple(part.strip() for part in value.split(",") if part.strip())
    if not tools:
        raise argparse.ArgumentTypeError("tool list must not be empty")
    unknown = [tool for tool in tools if tool not in allowed]
    if unknown:
        raise argparse.ArgumentTypeError(
            f"unknown tool(s): {', '.join(unknown)}; expected one of ripline, rg, grep"
        )
    return tools


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", help="Git repository to benchmark")
    parser.add_argument(
        "--preset",
        help="Named benchmark preset from the preset catalog",
    )
    parser.add_argument(
        "--preset-file",
        default=str(DEFAULT_PRESET_FILE),
        help="JSON preset catalog to load, default: benchmarks/repo_presets.json",
    )
    parser.add_argument(
        "--list-presets",
        action="store_true",
        help="List available presets and exit",
    )
    parser.add_argument(
        "--ripline-bin",
        default=str(DEFAULT_RIPLINE_BIN),
        help="Path to a ripline binary, default: target/release/ripline",
    )
    parser.add_argument(
        "--query",
        action="append",
        type=parse_query,
        default=[],
        help="Query spec, for example literal:workspace or regex:LanguageServer(Id|Status)",
    )
    parser.add_argument(
        "--build-iterations",
        type=int,
        default=3,
        help="Number of ripline index builds to time",
    )
    parser.add_argument(
        "--search-iterations",
        type=int,
        default=5,
        help="Number of search iterations per tool and query",
    )
    parser.add_argument(
        "--warmups",
        type=int,
        default=1,
        help="Warmup runs before timed search iterations",
    )
    parser.add_argument(
        "--grep-mode",
        choices=("tracked", "recursive"),
        default="tracked",
        help="tracked uses git ls-files, recursive uses grep -R over the repo root",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit machine-readable JSON instead of Markdown",
    )
    parser.add_argument(
        "--tools",
        type=parse_tools,
        help="Comma-separated tool set, for example ripline,rg or ripline,rg,grep",
    )
    args = parser.parse_args()

    preset_file = Path(args.preset_file).resolve()
    presets = load_presets(preset_file) if preset_file.exists() else {}
    if args.list_presets:
        if not presets:
            raise SystemExit(f"no preset catalog found at {preset_file}")
        print_presets(presets)
        return 0

    selected_preset = None
    if args.preset:
        if args.preset not in presets:
            known = ", ".join(sorted(presets))
            raise SystemExit(f"unknown preset {args.preset!r}. Known presets: {known}")
        selected_preset = presets[args.preset]

    repo_arg = args.repo
    if selected_preset and repo_arg is None:
        suggested_path = Path(selected_preset.suggested_local_path)
        if suggested_path.joinpath(".git").exists():
            repo_arg = str(suggested_path)
        else:
            raise SystemExit(
                f"preset {selected_preset.name!r} suggests {suggested_path}, "
                "but that repository is not present locally. Pass --repo explicitly."
            )

    if repo_arg is None:
        raise SystemExit("either --repo or --preset is required")

    repo_root = Path(repo_arg).resolve()
    ripline_bin = Path(args.ripline_bin).resolve()

    if not repo_root.joinpath(".git").exists():
        raise SystemExit(f"{repo_root} is not a Git repository")
    queries = list(args.query)
    if selected_preset and not queries:
        queries = list(selected_preset.queries)
    if not queries:
        raise SystemExit("at least one --query is required")

    build_iterations = args.build_iterations
    if selected_preset and args.build_iterations == parser.get_default("build_iterations"):
        build_iterations = selected_preset.build_iterations

    search_iterations = args.search_iterations
    if selected_preset and args.search_iterations == parser.get_default("search_iterations"):
        search_iterations = selected_preset.search_iterations

    warmups = args.warmups
    if selected_preset and args.warmups == parser.get_default("warmups"):
        warmups = selected_preset.warmups

    tools = args.tools
    if selected_preset and tools is None:
        tools = selected_preset.tools
    if tools is None:
        tools = ("ripline", "rg", "grep")

    ensure_ripline_binary(ripline_bin)

    env = dict(os.environ)
    env.setdefault("LC_ALL", "C")

    tracked = tracked_files(repo_root)
    tracked_count = sum(1 for part in tracked.split(b"\0") if part) if tracked else 0

    build_samples: list[float] = []
    for _ in range(build_iterations):
        with tempfile.TemporaryDirectory(prefix="ripline-bench-index-") as index_dir:
            cmd = [
                str(ripline_bin),
                "--repo-root",
                str(repo_root),
                "--index-dir",
                index_dir,
                "index",
                "--quiet",
            ]
            build_samples.append(
                run_timed(cmd, cwd=repo_root, env=env, allowed_codes=(0,))
            )

    with tempfile.TemporaryDirectory(prefix="ripline-bench-search-") as index_dir:
        index_path = Path(index_dir)
        subprocess.run(
            [
                str(ripline_bin),
                "--repo-root",
                str(repo_root),
                "--index-dir",
                str(index_path),
                "index",
                "--quiet",
            ],
            cwd=repo_root,
            env=env,
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        with tempfile.NamedTemporaryFile(prefix="ripline-bench-files-", delete=False) as filelist:
            filelist.write(tracked)
            tracked_list_path = Path(filelist.name)

        try:
            query_results: list[dict[str, object]] = []
            for query in queries:
                ripline_cmd = ripline_search_cmd(ripline_bin, repo_root, index_path, query)
                rg_cmd = rg_search_cmd(repo_root, query)
                grep_cmd = grep_search_cmd(repo_root, tracked_list_path, query, args.grep_mode)

                counts: dict[str, int] = {}
                timings: dict[str, dict[str, float]] = {}
                if "ripline" in tools:
                    counts["ripline"] = output_line_count(
                        ripline_cmd, cwd=repo_root, env=env, allowed_codes=(0, 1)
                    )
                    timings["ripline"] = benchmark_command(
                        ripline_cmd,
                        cwd=repo_root,
                        env=env,
                        warmups=warmups,
                        iterations=search_iterations,
                        allowed_codes=(0, 1),
                    )
                if "rg" in tools:
                    counts["rg"] = output_line_count(
                        rg_cmd, cwd=repo_root, env=env, allowed_codes=(0, 1)
                    )
                    timings["rg"] = benchmark_command(
                        rg_cmd,
                        cwd=repo_root,
                        env=env,
                        warmups=warmups,
                        iterations=search_iterations,
                        allowed_codes=(0, 1),
                    )
                if "grep" in tools:
                    counts["grep"] = output_line_count(
                        grep_cmd,
                        cwd=repo_root,
                        env=env,
                        shell=True,
                        allowed_codes=(0, 1, 123),
                    )
                    timings["grep"] = benchmark_command(
                        grep_cmd,
                        cwd=repo_root,
                        env=env,
                        warmups=warmups,
                        iterations=search_iterations,
                        shell=True,
                        allowed_codes=(0, 1, 123),
                    )

                query_results.append(
                    {
                        "query": query.name,
                        "counts": counts,
                        "timings_ms": timings,
                    }
                )
        finally:
            tracked_list_path.unlink(missing_ok=True)

    report = {
        "repo": str(repo_root),
        "preset": selected_preset.name if selected_preset else None,
        "tracked_files": tracked_count,
        "grep_mode": args.grep_mode,
        "tools": list(tools),
        "build_iterations": build_iterations,
        "search_iterations": search_iterations,
        "warmups": warmups,
        "ripline_index_build_ms": summarize(build_samples),
        "queries": query_results,
    }

    if args.json:
        print(json.dumps(report, indent=2))
        return 0

    print(f"# External Benchmark\n")
    print(f"- Repo: `{report['repo']}`")
    if report["preset"]:
        print(f"- Preset: `{report['preset']}`")
    print(f"- Tracked files: `{report['tracked_files']}`")
    print(f"- Grep mode: `{report['grep_mode']}`")
    print(f"- Tools: `{', '.join(report['tools'])}`")
    print(f"- Ripline build iterations: `{report['build_iterations']}`")
    print(f"- Search iterations per tool/query: `{report['search_iterations']}`\n")

    build_summary = report["ripline_index_build_ms"]
    print("## Ripline index build\n")
    print(
        f"- median: `{build_summary['median_ms']}` ms"
        f", min: `{build_summary['min_ms']}` ms"
        f", max: `{build_summary['max_ms']}` ms\n"
    )

    print("## Search latency\n")
    print("| Query | Tool | Matches | Median ms | Min ms | Max ms |")
    print("|---|---:|---:|---:|---:|---:|")
    for result in report["queries"]:
        query_name = result["query"]
        counts = result["counts"]
        timings = result["timings_ms"]
        for tool in report["tools"]:
            summary = timings[tool]
            print(
                f"| `{query_name}` | `{tool}` | `{counts[tool]}` | "
                f"`{summary['median_ms']}` | `{summary['min_ms']}` | `{summary['max_ms']}` |"
            )

    print("\n## Notes\n")
    print(
        "- `ripline` search latency excludes index build time, which is reported separately."
    )
    if args.grep_mode == "tracked":
        print(
            "- `grep` uses `git ls-files` as its file list. That is a better baseline than raw recursive grep, but it is still not ignore-aware in the same way as `rg`."
        )
    else:
        print("- `grep` uses recursive traversal and may include files that `rg` or `ripline` skip.")

    mismatched = [
        result
        for result in report["queries"]
        if len(set(result["counts"].values())) != 1
    ]
    if mismatched:
        print("- Match counts differ for at least one query. Treat timing comparisons cautiously.")
        literal_mismatches = [
            result
            for result in mismatched
            if str(result["query"]).startswith("literal:")
        ]
        if literal_mismatches:
            print(
                "- For literal queries, a lower `ripline` count often means the pattern is being matched as a mid-token substring inside larger identifiers. Current `ripline` coverage guarantees are strongest for token-aligned queries."
            )

    return 0


if __name__ == "__main__":
    sys.exit(main())
