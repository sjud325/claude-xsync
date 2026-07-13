#!/usr/bin/env python3
"""Structure-preserving fixture masking for claude-xsync golden tests.

Usage: mask-fixture.py <in.jsonl> <out.jsonl>

Parses each line as JSON and walks every string (keys and values):
- alphanumeric runs of >= 4 chars are replaced with same-length 'x' runs,
  Hangul runs with same-length '한' runs (content is destroyed),
- EXCEPT inside path-shaped substrings that start with a home-path prefix
  (/Users/<name> or <drive>:\\Users\\<name>): those are kept verbatim —
  they are the test subject.
Non-JSON lines (e.g. a crash-truncated tail) are masked raw and passed
through so the fixture still exercises the line-level fail-closed path.
The output always re-parses as JSONL and preserves every path-shaped
substring byte-for-byte.
"""
import json
import re
import sys

HOME_RE = re.compile(r"(?:/Users/[A-Za-z0-9_.-]+|[A-Za-z]:[\\/][Uu]sers[\\/][A-Za-z0-9_.-]+)")
RUN_CHAR = re.compile(r"[A-Za-z0-9_\-./\\~+@]|[^\x00-\x7F]")


def protected_spans(s: str):
    """Byte spans of home-prefixed path runs (kept verbatim)."""
    spans = []
    for m in HOME_RE.finditer(s):
        end = m.end()
        while end < len(s) and RUN_CHAR.match(s[end]):
            end += 1
        spans.append((m.start(), end))
    return spans


def mask_segment(seg: str) -> str:
    seg = re.sub(r"[A-Za-z0-9]{4,}", lambda m: "x" * len(m.group()), seg)
    seg = re.sub(r"[가-힣]+", lambda m: "한" * len(m.group()), seg)
    return seg


def mask_string(s: str) -> str:
    out, pos = [], 0
    for start, end in protected_spans(s):
        if start < pos:
            continue
        out.append(mask_segment(s[pos:start]))
        out.append(s[start:end])
        pos = end
    out.append(mask_segment(s[pos:]))
    return "".join(out)


def walk(v):
    if isinstance(v, str):
        return mask_string(v)
    if isinstance(v, list):
        return [walk(x) for x in v]
    if isinstance(v, dict):
        out = {}
        for i, (k, val) in enumerate(v.items()):
            mk = mask_string(k)
            if mk in out:  # same-length masks collide — keep length, disambiguate
                suffix = str(i)
                mk = mk[: -len(suffix)] + suffix if len(mk) >= len(suffix) else mk + suffix
            out[mk] = walk(val)
        return out
    return v


def main(inp: str, outp: str) -> None:
    total = parsed = raw = 0
    with open(inp, encoding="utf-8") as f, open(outp, "w", encoding="utf-8") as g:
        for line in f:
            total += 1
            body = line.rstrip("\n")
            nl = "\n" if line.endswith("\n") else ""
            try:
                obj = json.loads(body)
            except json.JSONDecodeError:
                g.write(mask_string(body) + nl)
                raw += 1
                continue
            g.write(json.dumps(walk(obj), ensure_ascii=False, separators=(",", ":")) + nl)
            parsed += 1
    print(f"{total} lines: {parsed} masked structurally, {raw} masked raw", file=sys.stderr)


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
