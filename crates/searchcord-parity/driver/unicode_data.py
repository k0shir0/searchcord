"""Regenerate normative Unicode 15 tables used by profile label normalization.

This produces data only, not a production Python dependency or test adapter.
Run with Python 3.12, whose unicodedata version matches the pinned reference.
"""
import json
from pathlib import Path
import unicodedata

if unicodedata.unidata_version != '15.0.0':
    raise RuntimeError('Regeneration requires Unicode 15.0.0 (Python 3.12)')
directory = Path(__file__).resolve().parents[2] / 'searchcord-archive' / 'src'
casefold, ranges = [], []
start = previous = None
for codepoint in range(0x110000):
    char = chr(codepoint)
    if char.casefold() != char:
        casefold.append([codepoint, char.casefold()])
    if unicodedata.category(char)[0] in 'LNM':
        if start is None:
            start = codepoint
        previous = codepoint
    elif start is not None:
        ranges.append([start, previous])
        start = previous = None
if start is not None:
    ranges.append([start, previous])
for name, data in [('casefold-15.json', casefold), ('label-categories-15.json', ranges)]:
    (directory / name).write_text(json.dumps(data, ensure_ascii=True, separators=(',', ':')) + '\n', encoding='utf-8')
digits = []
start = previous = None
for codepoint in range(0x110000):
    if chr(codepoint).isdigit():
        if start is None:
            start = codepoint
        previous = codepoint
    elif start is not None:
        digits.append([start, previous])
        start = previous = None
if start is not None:
    digits.append([start, previous])
(directory / 'digits-15.json').write_text(json.dumps(digits, separators=(',', ':')) + '\n', encoding='utf-8')
