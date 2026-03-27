# Quickstart: Ripline

## Prerequisites

- Rust 1.75+ (`rustup update stable`)
- A Git repository to index

## Build

```bash
cargo build --release
```

## Index a Repository

```bash
cd /path/to/your/repo
ripline index --stats
```

This builds the full n-gram index in `.ripline/`. Typical time: 1-3 seconds for repositories under 500k LOC.

## Search

```bash
# Literal search
ripline search -l "fn parse_query"

# Regex search
ripline search "fn\s+\w+_query"

# Restrict to Rust files
ripline search -t rs "impl.*Iterator"

# Restrict to a path
ripline search "TODO" src/index/

# Case-insensitive
ripline search -i "error"

# JSON output (for tooling integration)
ripline search --json "fn main"
```

## Update After Edits

```bash
# Incremental update (fast, uses overlay)
ripline update

# Force full rebuild
ripline index --force
```

## Check Status

```bash
ripline status
```

## Use as a Library

```rust
use ripline::{Config, Index, SearchOptions};
use std::path::PathBuf;

let config = Config {
    repo_root: PathBuf::from("/path/to/repo"),
    index_dir: PathBuf::from("/path/to/repo/.ripline"),
    ..Config::default()
};

let index = Index::open(config)?;
index.build()?;

let results = index.search(
    "fn parse",
    &SearchOptions::default(),
)?;

for m in &results {
    println!("{}:{}: {}", m.path.display(), m.line_number, m.line_content);
}
```

## Agent Integration

Ripline is designed for AI agent workflows. Key properties:

- **Fast**: sub-50ms warm queries. Agents can grep repeatedly without stalling.
- **Fresh**: `notify_change()` updates the overlay instantly. Agents see code they just wrote.
- **Correct**: results always verified against the actual file content. No false positives.
- **Compatible**: output matches `grep -rn` format. Drop-in replacement for agent grep tools.
