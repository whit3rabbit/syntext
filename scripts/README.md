# scripts/

## weights_gen.py

Generates `src/tokenizer/weights.rs` — the bigram frequency weight table used by the sparse n-gram tokenizer. Do not edit `weights.rs` by hand.

### Prerequisites

**1. Install the HuggingFace CLI:**

```sh
curl -LsSf https://hf.co/cli/install.sh | bash
```

**2. Request dataset access:**

Visit https://huggingface.co/datasets/bigcode/the-stack-smol and accept the terms. Requires a free HuggingFace account.

**3. Authenticate:**

```sh
hf auth login
```

**4. Create a venv and install Python dependencies:**

```sh
python3 -m venv .venv
.venv/bin/pip install datasets numpy
```

### Usage

```sh
HF_DATASETS_CACHE=/tmp/hf_cache .venv/bin/python scripts/weights_gen.py \
  --output src/tokenizer/weights.rs
```

Downloads ~175MB across Rust, Python, JavaScript, Go, Java, C, and TypeScript samples. `cpp` is absent from the dataset and is skipped automatically.

### Verify the output

The script prints diagnostics on completion. Expected values:

- Common pairs (`'  '`, `'re'`, `'er'`): weight < 12000
- Rare pairs (`'qz'`, `'xj'`): weight > 35000
- Unseen pairs: weight 65535
- Non-zero pairs: ~15000+

If all weights are 65535, the script ran with no corpus. Check HF auth and dataset access.

### Local corpus alternative

If you cannot access the HuggingFace dataset, point the script at local source directories:

```sh
.venv/bin/python scripts/weights_gen.py \
  --source local \
  --dirs ~/projects/linux ~/projects/rustc ~/projects/cpython \
  --output src/tokenizer/weights.rs
```
