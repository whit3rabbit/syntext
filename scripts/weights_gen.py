"""
Generate a bigram frequency weight table for sparse n-gram tokenization.

This script:
1. Downloads a sample of open-source code from GitHub via the Stack dataset
2. Counts all 65,536 byte-pair frequencies on lowercased content
3. Inverts frequencies to weights (rare pairs = high weight)
4. Emits a Rust const array for src/tokenizer/weights.rs

Usage (Google Colab or local):
    pip install datasets
    python generate_weights.py

Output: weights.rs (copy into your project)
"""

import numpy as np
from collections import Counter
import struct
import os

# ---------------------------------------------------------------------------
# Option A: Use HuggingFace "the-stack-smol" dataset (easiest, ~500MB download)
# Option B: Clone a handful of popular repos and read files directly
# Option C: Use any local code corpus you already have
# ---------------------------------------------------------------------------

def count_bigrams_from_huggingface(target_bytes=200_000_000):
    """Download code samples from the-stack-smol and count bigram frequencies."""
    from datasets import load_dataset
    
    counts = np.zeros(65536, dtype=np.int64)
    total_bytes = 0
    
    # the-stack-smol has multiple language splits
    # Tier 1 languages + web/scripting/data for broader bigram coverage
    languages = [
        "rust", "python", "javascript", "go", "java", "c", "cpp", "typescript",
        "shell", "sql", "html", "css", "ruby", "php",
    ]
    bytes_per_lang = target_bytes // len(languages)
    
    for lang in languages:
        print(f"Processing {lang}...")
        lang_bytes = 0
        try:
            ds = load_dataset(
                "bigcode/the-stack-smol",
                data_dir=f"data/{lang}",
                split="train",
                streaming=True,
            )
            for sample in ds:
                content = sample["content"]
                if not content:
                    continue
                # Lowercase and encode to bytes
                lowered = content.lower().encode("utf-8", errors="replace")
                # Count consecutive byte pairs
                for i in range(len(lowered) - 1):
                    pair = (lowered[i] << 8) | lowered[i + 1]
                    counts[pair] += 1
                lang_bytes += len(lowered)
                total_bytes += len(lowered)
                if lang_bytes >= bytes_per_lang:
                    break
        except Exception as e:
            print(f"  Skipping {lang}: {e}")
            continue
        print(f"  {lang}: {lang_bytes / 1e6:.1f} MB processed")
    
    print(f"\nTotal: {total_bytes / 1e6:.1f} MB processed")
    return counts


def count_bigrams_from_local_dirs(dirs):
    """Count bigram frequencies from local code directories."""
    CODE_EXTENSIONS = {
        ".rs", ".py", ".js", ".ts", ".go", ".java", ".c", ".h",
        ".cpp", ".hpp", ".cc", ".rb", ".swift", ".kt", ".scala",
        ".cs", ".jsx", ".tsx", ".vue", ".sh", ".bash", ".zsh",
        ".toml", ".yaml", ".yml", ".json", ".md", ".txt",
    }
    
    counts = np.zeros(65536, dtype=np.int64)
    total_bytes = 0
    file_count = 0
    
    for root_dir in dirs:
        for dirpath, _, filenames in os.walk(root_dir):
            # Skip hidden dirs and common non-source dirs
            if any(part.startswith('.') for part in dirpath.split(os.sep)):
                continue
            if any(skip in dirpath for skip in ["node_modules", "target", "vendor", "__pycache__", ".git"]):
                continue
            
            for fname in filenames:
                ext = os.path.splitext(fname)[1].lower()
                if ext not in CODE_EXTENSIONS:
                    continue
                
                fpath = os.path.join(dirpath, fname)
                try:
                    with open(fpath, "rb") as f:
                        raw = f.read(1_000_000)  # Cap at 1MB per file
                    # Lowercase ASCII bytes (leave non-ASCII as-is)
                    lowered = bytes(
                        b + 32 if 65 <= b <= 90 else b for b in raw
                    )
                    for i in range(len(lowered) - 1):
                        pair = (lowered[i] << 8) | lowered[i + 1]
                        counts[pair] += 1
                    total_bytes += len(lowered)
                    file_count += 1
                except (OSError, UnicodeDecodeError):
                    continue
    
    print(f"Processed {file_count} files, {total_bytes / 1e6:.1f} MB")
    return counts


def counts_to_weights(counts):
    """
    Convert bigram frequency counts to weights.
    
    Strategy: rare pairs get HIGH weight (they make good gram boundaries).
    Common pairs get LOW weight (they should be in the interior of grams).
    
    We want:
    - Weights in u16 range [0, 65535]
    - Zero-count pairs get maximum weight (they're maximally rare)
    - The most common pair gets weight ~1
    - Log-scale inversion to compress the dynamic range
    """
    weights = np.zeros(65536, dtype=np.float64)
    
    # Add 1 to avoid log(0), then take log
    log_counts = np.log1p(counts.astype(np.float64))
    
    # Invert: high count -> low weight, low count -> high weight
    max_log = log_counts.max()
    if max_log > 0:
        # Normalize to [0, 1] where 1 = most common
        normalized = log_counts / max_log
        # Invert and scale to u16 range
        # Most common pair -> weight ~100 (not zero, so it can still be a boundary if needed)
        # Rarest pair -> weight ~65000
        weights = ((1.0 - normalized) * 64900 + 100).astype(np.float64)
    
    # Zero-count pairs get maximum weight
    weights[counts == 0] = 65535
    
    # Clamp to u16
    weights = np.clip(weights, 0, 65535).astype(np.uint16)
    
    return weights


def emit_rust_const(weights, output_path="weights.rs"):
    """Write the weights as a Rust const array."""
    with open(output_path, "w") as f:
        f.write("// Auto-generated by generate_weights.py\n")
        f.write("// Bigram frequency weights for sparse n-gram tokenization.\n")
        f.write("// Rare byte-pairs get high weights (good gram boundaries).\n")
        f.write("// Common byte-pairs get low weights (gram interiors).\n")
        f.write("//\n")
        f.write("// Index: (byte_a << 8) | byte_b\n")
        f.write("// Usage: BIGRAM_WEIGHTS[(b1 as usize) << 8 | (b2 as usize)]\n")
        f.write(f"// Corpus: mixed open-source code (Rust, Python, JS, Go, Java, C/C++, TS)\n")
        f.write("\n")
        f.write("pub const BIGRAM_WEIGHTS: [u16; 65536] = [\n")
        
        for i in range(0, 65536, 16):
            row = ", ".join(f"{weights[i+j]:5}" for j in range(16))
            f.write(f"    {row},\n")
        
        f.write("];\n")
    
    print(f"Written to {output_path}")
    print(f"File size: {os.path.getsize(output_path) / 1024:.0f} KB")


def print_diagnostics(counts, weights):
    """Print useful info about the weight distribution."""
    print("\n=== Diagnostics ===\n")
    
    # Top 20 most common byte pairs
    top_indices = np.argsort(counts)[::-1][:20]
    print("Top 20 most common byte pairs (LOW weight = gram interior):")
    for idx in top_indices:
        b1, b2 = idx >> 8, idx & 0xFF
        c1 = chr(b1) if 32 <= b1 < 127 else f"\\x{b1:02x}"
        c2 = chr(b2) if 32 <= b2 < 127 else f"\\x{b2:02x}"
        print(f"  '{c1}{c2}'  count={counts[idx]:>12,}  weight={weights[idx]:>5}")
    
    # Top 20 rarest (non-zero) byte pairs
    nonzero = counts > 0
    rare_indices = np.argsort(counts + (1 - nonzero.astype(np.int64)) * 10**18)[:20]
    print("\nTop 20 rarest byte pairs (HIGH weight = good gram boundaries):")
    for idx in rare_indices:
        if counts[idx] == 0:
            continue
        b1, b2 = idx >> 8, idx & 0xFF
        c1 = chr(b1) if 32 <= b1 < 127 else f"\\x{b1:02x}"
        c2 = chr(b2) if 32 <= b2 < 127 else f"\\x{b2:02x}"
        print(f"  '{c1}{c2}'  count={counts[idx]:>12,}  weight={weights[idx]:>5}")
    
    # Weight distribution
    print(f"\nWeight stats:")
    print(f"  Min weight: {weights.min()}")
    print(f"  Max weight: {weights.max()}")
    print(f"  Median weight: {np.median(weights):.0f}")
    print(f"  Zero-count pairs (weight=65535): {(counts == 0).sum()}")
    print(f"  Non-zero pairs: {(counts > 0).sum()}")
    
    # Sanity check: common code patterns should have LOW weights
    print("\nSanity check (common patterns should have LOW weights):")
    test_pairs = [
        (ord('e'), ord(' ')),   # 'e ' - very common
        (ord('r'), ord('e')),   # 're' - common in 'return', 'result'
        (ord('f'), ord('n')),   # 'fn' - Rust keyword
        (ord('_'), ord('_')),   # '__' - Python dunder
        (ord('q'), ord('z')),   # 'qz' - rare
        (ord('x'), ord('j')),   # 'xj' - rare
    ]
    for b1, b2 in test_pairs:
        idx = (b1 << 8) | b2
        c1 = chr(b1) if 32 <= b1 < 127 else f"\\x{b1:02x}"
        c2 = chr(b2) if 32 <= b2 < 127 else f"\\x{b2:02x}"
        print(f"  '{c1}{c2}'  weight={weights[idx]:>5}  ({'LOW/common' if weights[idx] < 10000 else 'HIGH/rare'})")


def main():
    import argparse
    parser = argparse.ArgumentParser(description="Generate sparse n-gram weight table")
    parser.add_argument("--source", choices=["huggingface", "local"], default="huggingface",
                        help="Data source: 'huggingface' (download the-stack-smol) or 'local' (scan local dirs)")
    parser.add_argument("--dirs", nargs="*", default=[],
                        help="Local directories to scan (only used with --source local)")
    parser.add_argument("--target-mb", type=int, default=200,
                        help="Target corpus size in MB (for huggingface source)")
    parser.add_argument("--output", default="weights.rs",
                        help="Output file path")
    args = parser.parse_args()
    
    # Step 1: Count bigrams
    if args.source == "huggingface":
        counts = count_bigrams_from_huggingface(target_bytes=args.target_mb * 1_000_000)
    else:
        if not args.dirs:
            print("Error: --dirs required when using --source local")
            print("Example: python generate_weights.py --source local --dirs ~/projects/linux ~/projects/rustc")
            return
        counts = count_bigrams_from_local_dirs(args.dirs)
    
    # Step 2: Convert to weights
    weights = counts_to_weights(counts)
    
    # Step 3: Diagnostics
    print_diagnostics(counts, weights)
    
    # Step 4: Emit Rust const
    emit_rust_const(weights, args.output)
    
    print(f"\nDone! Copy {args.output} to src/tokenizer/weights.rs")


if __name__ == "__main__":
    main()
