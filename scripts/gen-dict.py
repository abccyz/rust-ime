#!/usr/bin/env python3
"""Generate crates/ime-engine/data/dict.txt from a real word-frequency list.

Sources:
  * jieba's dict.txt (word + frequency), MIT licensed;
  * pypinyin for toneless pinyin (context-aware for 多音字), MIT licensed.

Usage:
    python3 scripts/gen-dict.py [--source /path/to/jieba-dict.txt] [--max 60000]
"""

import argparse
import math
import re
import subprocess
import sys
from pathlib import Path

import pypinyin
from pypinyin import Style, lazy_pinyin

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_SRC = ROOT / "target" / "dictgen" / "jieba.txt"
OUT = ROOT / "crates" / "ime-engine" / "data" / "dict.txt"
URL = "https://gh-proxy.com/https://raw.githubusercontent.com/fxsjy/jieba/master/jieba/dict.txt"

CJK = re.compile(r"^[\u4e00-\u9fff]+$")


def ensure_source(path: Path) -> None:
    if path.exists():
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    print(f"downloading word list -> {path}")
    subprocess.run(["curl", "-sL", URL, "-o", str(path)], check=True)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", type=Path, default=DEFAULT_SRC)
    ap.add_argument("--max", type=int, default=60000)
    args = ap.parse_args()

    ensure_source(args.source)

    entries = []
    with args.source.open(encoding="utf-8") as f:
        for line in f:
            parts = line.rstrip("\n").split(" ")
            if len(parts) < 2:
                continue
            word, freq_s = parts[0], parts[1]
            try:
                freq = int(freq_s)
            except ValueError:
                continue
            if not CJK.match(word) or not (1 <= len(word) <= 4):
                continue
            entries.append((freq, word))

    entries.sort(reverse=True)
    seen = set()
    out = []
    for freq, word in entries[: args.max * 3]:
        syls = lazy_pinyin(word, style=Style.NORMAL, errors="ignore")
        if len(syls) != len(word):
            continue
        pinyin = "".join(s.replace("v", "u") for s in syls)
        if not re.fullmatch(r"[a-z]{1,40}", pinyin):
            continue
        key = (pinyin, word)
        if key in seen:
            continue
        seen.add(key)
        score = min(9.0, 3.0 + math.log10(max(1, freq)) * 1.2)
        out.append((pinyin, word, score))
        if len(out) >= args.max:
            break

    OUT.write_text(
        "".join(f"{p}\t{w}\t{s:.2f}\n" for p, w, s in out), encoding="utf-8"
    )
    print(f"wrote {len(out)} entries to {OUT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
