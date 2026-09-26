#!/usr/bin/env python3
from itertools import permutations

# Bounded dependency DAG: users before posts; posts before comments.
deps={"users":set(),"posts":{"users"},"comments":{"posts"}}
valid=[]
for order in permutations(deps):
  seen=set(); ok=True
  for node in order:
    if not deps[node] <= seen: ok=False; break
    seen.add(node)
  if ok: valid.append(order)
assert valid==[("users","posts","comments")], "migration partial order is ambiguous or violated"
# Applying the same migration id twice is idempotent in the abstract applied-set model.
applied={"users"}; assert applied|{"users"}==applied
print("migration partial-order/idempotency model: ok")
