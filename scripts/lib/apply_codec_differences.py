"""Apply the corpus's recorded differences to the host's answers.

A corpus line may say the producer refuses where the host converts, with the
message it must give -- WebKit has no GBK encoder, and that is a fact about the
platform rather than a disagreement to fix. Applying it here keeps the diff
asserting exactly those differences: a producer that answered instead, or
refused with another message, still fails.
"""

import sys

corpus, host = sys.argv[1], sys.argv[2]
expected = {}
for line in open(corpus, encoding="utf-8"):
    if line.startswith("#") or not line.strip():
        continue
    fields = line.rstrip("\n").split("\t")
    for field in fields[3:]:
        if field.startswith("producer-refuses:"):
            expected["\t".join(fields[:3])] = field[len("producer-refuses:"):]

rewritten = []
for line in open(host, encoding="utf-8"):
    fields = line.rstrip("\n").split("\t")
    key = "\t".join(fields[:3])
    if key in expected:
        rewritten.append("\t".join(fields[:-2] + ["refused", expected[key]]))
    else:
        rewritten.append(line.rstrip("\n"))
open(host, "w", encoding="utf-8").write("\n".join(rewritten) + "\n")
print(f"applied {len(expected)} recorded difference(s)")
