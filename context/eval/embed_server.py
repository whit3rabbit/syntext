#!/usr/bin/env python3
"""Sentence-embedding server for the eval runner (`--embed`).

Speaks JSON lines on stdin/stdout. On start it prints
`{"id": ..., "dims": ...}`; then for each request `{"texts": [...],
"query": bool}` it prints `{"vectors": [[...], ...]}` (or `{"error": ...}`).

Runs a BERT-style ONNX encoder with CLS pooling and L2 normalization, as
the BGE models expect, via onnxruntime and tokenizers:

    python3 context/eval/embed_server.py --model-dir models/bge-small-en-v1.5

The model directory needs `model.onnx` and `tokenizer.json` (for example
from https://huggingface.co/BAAI/bge-small-en-v1.5, `onnx/model.onnx`).
"""

import argparse
import json
import os
import sys

import numpy as np
import onnxruntime as ort
from tokenizers import Tokenizer

BGE_QUERY = "Represent this sentence for searching relevant passages: "


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--model-dir", required=True)
    ap.add_argument("--id", help="embedder id (default: the directory name)")
    ap.add_argument("--query-prefix", default=BGE_QUERY)
    ap.add_argument("--max-length", type=int, default=512)
    ap.add_argument("--threads", type=int, default=0, help="onnxruntime intra-op threads (0: all)")
    args = ap.parse_args()

    tok = Tokenizer.from_file(os.path.join(args.model_dir, "tokenizer.json"))
    tok.enable_truncation(max_length=args.max_length)
    tok.enable_padding(pad_id=0, pad_token="[PAD]")
    so = ort.SessionOptions()
    so.intra_op_num_threads = args.threads
    sess = ort.InferenceSession(os.path.join(args.model_dir, "model.onnx"), so,
                                providers=["CPUExecutionProvider"])
    inputs = {i.name for i in sess.get_inputs()}

    def embed(texts):
        enc = tok.encode_batch(texts)
        ids = np.array([e.ids for e in enc], dtype=np.int64)
        mask = np.array([e.attention_mask for e in enc], dtype=np.int64)
        feeds = {"input_ids": ids, "attention_mask": mask}
        if "token_type_ids" in inputs:
            feeds["token_type_ids"] = np.zeros_like(ids)
        cls = sess.run(None, feeds)[0][:, 0]
        return cls / np.maximum(np.linalg.norm(cls, axis=1, keepdims=True), 1e-12)

    dims = int(embed(["probe"]).shape[1])
    ident = args.id or os.path.basename(os.path.normpath(args.model_dir))
    print(json.dumps({"id": ident, "dims": dims}), flush=True)
    for line in sys.stdin:
        if not line.strip():
            continue
        try:
            req = json.loads(line)
            texts = req["texts"]
            if req.get("query"):
                texts = [args.query_prefix + t for t in texts]
            out = {"vectors": embed(texts).round(6).tolist()} if texts else {"vectors": []}
        except Exception as e:  # report and keep serving
            out = {"error": f"{type(e).__name__}: {e}"}
        print(json.dumps(out), flush=True)


if __name__ == "__main__":
    main()
