# CLI Contract

**Date**: 2026-03-25
**Type**: Command-line interface

## Binary Name

`st`

## Commands

### `st search <PATTERN> [PATH...]`

Search for a pattern in the indexed repository.

```
USAGE:
    st search [OPTIONS] <PATTERN> [PATH...]

ARGS:
    <PATTERN>    Regex pattern to search for
    [PATH...]    Restrict search to these paths (globs supported)

OPTIONS:
    -l, --literal          Treat pattern as a literal string (not regex)
    -i, --ignore-case      Case-insensitive search
    -t, --type <TYPE>      Restrict to file type (e.g., rs, py, js)
    -T, --type-not <TYPE>  Exclude file type
    -m, --max-count <N>    Maximum results to return
    -c, --count            Show only match count per file
        --json             Output results as JSON (one object per line)
    -q, --quiet            Suppress output, exit 0 if match found, 1 if not
```

**Output format** (default):
```
src/tokenizer/mod.rs:42:    let weight = weights[pair_hash];
src/tokenizer/mod.rs:58:    let covering = build_covering(&query, &weights);
src/index/segment.rs:115:   let entry = DictEntry { gram_hash, offset };
```

**Output format** (--json):
```json
{"path":"src/tokenizer/mod.rs","line":42,"content":"    let weight = weights[pair_hash];","byte_offset":1823}
```

**Exit codes**:
- 0: matches found
- 1: no matches found
- 2: error (invalid pattern, corrupt index, I/O error)

### `st index [OPTIONS]`

Build or rebuild the index.

```
USAGE:
    st index [OPTIONS]

OPTIONS:
    --force        Rebuild from scratch even if index exists
    --stats        Print index statistics after build
    --quiet        Suppress progress output
```

### `st status`

Show index status and statistics.

```
USAGE:
    st status [OPTIONS]

OPTIONS:
    --json         Output as JSON
```

**Output**:
```
Index: /path/to/repo/.syntext/
Documents: 1,234
Segments: 3
Grams: 89,012
Size: 12.4 MB
Base commit: abc1234
Overlay: 5 dirty files
```

### `st update`

Incrementally update the index for changed files since last build.

```
USAGE:
    st update [OPTIONS]

OPTIONS:
    --flush        Force flush overlay to segment
    --quiet        Suppress output
```

## Global Options

```
OPTIONS:
    --index-dir <DIR>   Override index directory (default: .syntext/)
    --repo-root <DIR>   Override repository root (default: auto-detect via .git)
    -v, --verbose        Increase verbosity
    -h, --help           Print help
    -V, --version        Print version
```

## Environment Variables

| Variable | Description | Default |
|---|---|---|
| SYNTEXT_INDEX_DIR | Index directory path | `{repo_root}/.syntext/` |
| SYNTEXT_MAX_FILE_SIZE | Max file size to index (bytes) | 10485760 (10MB) |

## Compatibility Notes

- Output format for `search` is designed to be compatible with `grep -rn` style output for easy integration with editors and tools.
- `--json` output uses newline-delimited JSON for streaming compatibility.
- Exit codes follow grep convention (0 = match, 1 = no match, 2 = error).
