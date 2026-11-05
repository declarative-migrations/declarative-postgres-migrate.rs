#!/usr/bin/env python3
"""Finite migration lease model; not a PostgreSQL or AttoLang execution proof.

Enumerates acquisition, competing writers, loss, revival and stale tickets.
Database lease and transaction evidence is still required separately.
"""
from collections import deque
from dataclasses import dataclass, replace
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ACTORS = ('a', 'b')


@dataclass(frozen=True)
class State:
    holder: str = '-'
    epoch: int = 0
    ticket_a: int = 0
    ticket_b: int = 0
    validated: bool = True
    writes: int = 0
    denied: int = 0


def authorized(s: State, actor: str) -> bool:
    ticket = s.ticket_a if actor == 'a' else s.ticket_b
    return (s.holder == actor and s.epoch > 0
            and ticket == s.epoch and s.validated)


def step(s: State, action: str) -> tuple[State, bool]:
    if action.startswith('acquire_'):
        a = action[-1]
        if s.holder == '-' and s.epoch < 3:
            epoch = s.epoch + 1
            key = 'ticket_a' if a == 'a' else 'ticket_b'
            return replace(s, holder=a, epoch=epoch, **{key: epoch}), True
        return s, False
    if action == 'expire':
        return replace(s, holder='-'), True
    if action == 'invalidate':
        return replace(s, validated=False), True
    if action == 'validate':
        return replace(s, validated=True), True
    if action.startswith('release_'):
        a = action[-1]
        if authorized(s, a):
            return replace(s, holder='-'), True
        return s, False
    if action.startswith('apply_'):
        a = action[-1]
        admitted = authorized(s, a)
        return (replace(s, writes=min(s.writes + 1, 2)) if admitted else
                replace(s, denied=min(s.denied + 1, 2))), admitted
    raise ValueError(action)


def main() -> int:
    events = tuple(f'{v}_{a}' for v in ('acquire', 'release', 'apply')
                   for a in ACTORS) + ('expire', 'invalidate', 'validate')
    init = State()
    visited, queue = {init}, deque([init])
    witnesses = set()
    transitions = 0
    while queue:
        s = queue.popleft()
        assert 0 <= s.epoch <= 3 and s.holder in ('-', *ACTORS)
        for action in events:
            t, admitted = step(s, action)
            transitions += 1
            assert t.epoch >= s.epoch, 'lease epoch must not roll back'
            assert t.writes >= s.writes and t.writes <= 2
            if action.startswith('apply_'):
                a = action[-1]
                assert admitted == authorized(s, a), 'write without active lease'
                assert t.writes - s.writes == (1 if admitted and s.writes < 2 else 0)
                if admitted:
                    witnesses.add('write')
                else:
                    if s.holder != a and s.holder != '-':
                        witnesses.add('contention')
                    if not s.validated:
                        witnesses.add('invalid')
                    if s.holder == '-' and (s.ticket_a or s.ticket_b):
                        witnesses.add('expired')
                    ticket = s.ticket_a if a == 'a' else s.ticket_b
                    if ticket and ticket < s.epoch:
                        witnesses.add('stale_epoch')
            else:
                assert t.writes == s.writes, f'{action} mutated database writes'
            if t not in visited:
                visited.add(t)
                queue.append(t)
    assert len(visited) > 50 and transitions > 200, 'vacuous model'
    assert {'write', 'contention', 'invalid', 'expired', 'stale_epoch'} <= witnesses, witnesses
    source = (ROOT / 'src/migration_lease_policy.atto').read_text()
    for anchor in ('define module migration_lease_policy', 'apply_denied(',
                   'claimed_epoch != current_epoch', '!owner_matches',
                   '!script_validated', 'self_check()'):
        assert anchor in source, anchor
    print(f'PASS finite lease safety: states={len(visited)} transitions={transitions}; '
          'AttoLang + DB NOT_EXECUTED')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
