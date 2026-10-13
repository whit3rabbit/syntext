#!/usr/bin/env python3
"""Download HotpotQA dev questions (distractor setting) for the eval runner.

The data is CC BY-SA 4.0 and is not stored in this repository. This script
fetches the first N validation questions from the Hugging Face dataset
viewer and writes them in HotpotQA's original JSON layout, which
`cargo run -p syntext-context --release --example eval -- --hotpot FILE`
reads.

    python3 context/eval/fetch_hotpot.py --limit 200 --out /tmp/hotpot_dev.json
"""

import argparse
import json
import urllib.request

API = ("https://datasets-server.huggingface.co/rows?dataset=hotpotqa/hotpot_qa"
       "&config=distractor&split=validation&offset={offset}&length={length}")


def fetch(offset, length):
    with urllib.request.urlopen(API.format(offset=offset, length=length), timeout=60) as r:
        return json.load(r)["rows"]


def convert(row):
    sf = row["supporting_facts"]
    ctx = row["context"]
    return {
        "_id": row["id"],
        "question": row["question"],
        "answer": row["answer"],
        "type": row["type"],
        "level": row["level"],
        "supporting_facts": [[t, i] for t, i in zip(sf["title"], sf["sent_id"])],
        "context": [[t, s] for t, s in zip(ctx["title"], ctx["sentences"])],
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--limit", type=int, default=200, help="questions to fetch")
    ap.add_argument("--out", required=True, help="output JSON file")
    args = ap.parse_args()
    items, offset = [], 0
    while len(items) < args.limit:
        rows = fetch(offset, min(100, args.limit - len(items)))
        if not rows:
            break
        items.extend(convert(r["row"]) for r in rows)
        offset += len(rows)
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(items, f, ensure_ascii=False)
    print(f"wrote {len(items)} questions to {args.out}")


if __name__ == "__main__":
    main()
