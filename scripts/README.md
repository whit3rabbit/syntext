# scripts/

## Weight table generation

`src/tokenizer/weights.rs` is auto-generated. Do not edit by hand.

### Recommended: Colab notebook (100 GB – 500 GB corpus)

[`notebooks/weights_gen_colab.ipynb`](notebooks/weights_gen_colab.ipynb) generates
weights from [`bigcode/the-stack-dedup`](https://huggingface.co/datasets/bigcode/the-stack-dedup)
(3 TB+, deduplicated, permissively licensed). Configurable target; 100 GB is a
reasonable default, 500 GB is the current shipped table.

**Corpus size tradeoffs:**

A 10 GB corpus leaves ~54% of byte pairs at zero (weight 65535). Many of those
zeros are real code patterns that simply did not appear in the sample, not
genuinely impossible combinations. At 100 GB, coverage reaches roughly ~45%
non-zero pairs. At ~500 GB (20+ languages), coverage reaches ~49.7%
(32,542 / 65,536 pairs). The remaining zeros are largely: ~4,264 printable ASCII
pairs (would improve with more data), ~6,249 control-char combos, and ~22,481
high-byte pairs (UTF-8 internals, effectively impossible in ASCII-dominant code).

The weight function uses log-scale inversion (`log1p`), so the difference between
500K occurrences (100 GB) and 5M occurrences (1 TB) is ~1 log unit, or ~3,000 on
a 65,000-point scale. Common patterns converge by ~10 GB; the long tail (unusual
identifiers, less common languages) benefits from more data.

| Target | Wall time (streaming ~3 MB/s) | Wall time (bulk Parquet) | Colab tier |
|--------|-------------------------------|--------------------------|------------|
| 10 GB  | ~55 min                       | ~5-10 min                | Free       |
| 50 GB  | ~4.5 hr                       | ~15-20 min               | Free       |
| 100 GB | ~9 hr                         | ~20-30 min               | Free       |
| 200 GB | ~18 hr                        | ~1 hr                    | Pro        |
| 500 GB | n/a                           | ~6.4 hr (22 MB/s actual) | Pro        |

The notebook uses bulk Parquet download with vectorized `np.bincount` (or CuPy on
GPU). Checkpointing after every shard means disconnects are recoverable.

**Setup:**

1. Visit https://huggingface.co/datasets/bigcode/the-stack-dedup and accept terms
2. Set Colab runtime to GPU (Runtime > Change runtime type > T4)
3. Add your HuggingFace token to Colab secrets (key sidebar > `HF_TOKEN`)
4. Run all cells

**Output:**

The notebook writes `weights.rs` and offers a browser download. Copy it into the
project:

```sh
cp ~/Downloads/weights.rs src/tokenizer/weights.rs
cargo test
cargo bench --bench query_latency -- --sample-size 10
```

**Expected diagnostics (500 GB run, current shipped table):**

- Common pairs (`'  '`, `'re'`, `'er'`): weight < 12000
- Rare pairs (`'qz'`, `'xj'`): weight > 28000 (boundary decisions correct)
- Unseen pairs: weight 65535
- Non-zero pairs: ~32,500+ (49.7% coverage)
- Unseen printable ASCII pairs: ~4,264 (would decrease with more data)
- Boundary checks on `fn`, `qu`, `st`, `re`: all PASS

The notebook also includes a convergence analysis cell that breaks down
zero-count pairs into printable ASCII (the ones that matter), control chars, and
high-byte (genuinely impossible combinations).

### Legacy: weights_gen.py (local, small corpus)

`weights_gen.py` generates weights from
[`bigcode/the-stack-smol`](https://huggingface.co/datasets/bigcode/the-stack-smol)
via streaming (~200 MB default, 7 languages). It produces a usable table but
with lower pair coverage (~46% at 10 GB). Use this only if Colab is unavailable.

**Prerequisites:**

```sh
python3 -m venv .venv
.venv/bin/pip install datasets numpy
hf auth login
```

**Usage:**

```sh
HF_DATASETS_CACHE=/tmp/hf_cache .venv/bin/python scripts/weights_gen.py \
  --output src/tokenizer/weights.rs
```

**Local corpus alternative:**

```sh
.venv/bin/python scripts/weights_gen.py \
  --source local \
  --dirs ~/projects/linux ~/projects/rustc ~/projects/cpython \
  --output src/tokenizer/weights.rs
```
