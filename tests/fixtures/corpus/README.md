# Fixture Corpus

Test fixture repository for ripline correctness testing.

## Structure

```
corpus/
  src/           Rust source files
  python/        Python source files
  typescript/    TypeScript/TSX files
  go/            Go source files
  java/          Java source files
  edge_cases/    Pathological files for edge case testing
  build/         .gitignored; should NOT be indexed
  .gitignore     Ignores build/ and common artifacts
```

## Invariants

These properties must hold for the ripline correctness test harness (T010):

- `parse_query` appears in: src/utils/parser.rs, python/query_engine.py,
  typescript/query.ts, go/search/client.go, java/SearchService.java,
  edge_cases/long_line.txt, edge_cases/no_newline_at_end.txt,
  edge_cases/repeated_pattern.txt, edge_cases/nested/deep/path/file.txt,
  edge_cases/special chars in name.txt  (>= 3 files required)

- `process_batch` appears in: python/query_engine.py, typescript/batch.ts,
  go/search/client.go, java/SearchService.java, edge_cases/long_line.txt,
  edge_cases/repeated_pattern.txt  (>= 2 files required)

- `ParseQuery` (capital P) appears in: typescript/query.ts, java/ParseQuery.java,
  java/SearchService.java, edge_cases/repeated_pattern.txt

- `parseQuery` (camelCase) appears in: typescript/query.ts,
  typescript/components/SearchBar.tsx, edge_cases/repeated_pattern.txt

- `PARSE_QUERY` (ALL CAPS) appears in: python/query_engine.py,
  typescript/query.ts, edge_cases/repeated_pattern.txt

- Email pattern (`user@domain.com`) appears in multiple files

- IPv4 pattern (`192.168.1.1`) appears in multiple files

- URL pattern (`https://example.com`) appears in multiple files

- `TODO` appears in: src/, python/, typescript/, java/, edge_cases/ (>= 5 files)

- `build/output.txt` must NOT appear in indexed results (gitignored)

- `edge_cases/empty.txt` is empty; indexer must not crash on it

- `edge_cases/no_newline_at_end.txt` has no trailing newline

- `edge_cases/crlf_endings.txt` uses CRLF line endings

- `edge_cases/long_line.txt` has a line >10K characters

- `edge_cases/special chars in name.txt` has spaces in filename

## Usage

```bash
# Run ripgrep oracle to capture expected results
rg parse_query tests/fixtures/corpus --ignore-file tests/fixtures/corpus/.gitignore

# Run ripline against same corpus
ripline search parse_query --index-dir /tmp/test-index --repo tests/fixtures/corpus
```
