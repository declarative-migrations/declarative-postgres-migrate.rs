# Declarative Postgres migration agent instructions

## Repository and migration invariants

- Treat desired schemas, migration planning, introspection, deparsing, dependency ordering, transaction boundaries, safety classification, and generated SQL as one coherent contract.
- Preserve idempotency and convergence: applying a generated plan and re-introspecting must produce the intended declarative state without repeated drift.
- Never silently drop, truncate, rewrite, or weaken constraints on user data. Destructive or irreversible operations require explicit classification, review, and tests.
- Keep PostgreSQL and explicitly supported compatible-database behavior covered separately; do not assume dialect equivalence.
- Release versions, Cargo metadata, trusted publishing, Homebrew formulas, checksums, tags, and documentation must remain synchronized.
- An ancestry-only merge strategy such as `-s ours` is not conflict resolution. It is acceptable only for explicitly superseded branches whose content is already present or intentionally obsolete, after comparing the branch, documenting why no content should enter, and verifying the current tree. Never use it to avoid reconciling live competing changes.

## Instruction discovery

Resolve `$PWD`, walk upward through every parent directory to the filesystem root, read every readable lowercase `agents.md` on that ancestor chain, and apply them root-to-leaf. Do not search siblings. Deduplicate resolved paths/inodes, avoid symlink cycles, and report unreadable files.

## Synchronize with the remote

Before editing, inspect `git status`, current branch, configured remotes, and the default branch. Run `git fetch --all --prune` and create the feature branch from the latest remote default branch, not a stale local branch. Fetch again before pushing and incorporate upstream changes according to repository policy. Never discard remote commits, force-push, rewrite shared history, bypass review, or bypass required CI.

## Resolve Git conflicts semantically

Resolve conflicts by understanding and combining both sides' intent. Do not mechanically choose `ours`, `theirs`, current, or incoming changes. Reconstruct the conceptually correct result while preserving compatible schema semantics, migration safety, dependency ordering, deparse/introspection convergence, SQL generation, tests, release metadata, documentation, configuration, and public APIs. Regenerate derived SQL, snapshots, lockfiles, formulas, or release artifacts from the merged source rather than selecting one side's generated output. If intentions are incompatible, make the smallest explicit design decision and document it in the pull request.

After resolving:

1. Reread every affected file from the top, not only the conflict hunks.
2. Run formatting, linting, unit/integration tests, database-version matrices, convergence tests, package/release dry runs, and relevant migration safety checks.
3. Search the entire worktree for unresolved conflict markers:

   ```sh
   grep -RInE '^(<<<<<<<|=======|>>>>>>>)' --exclude-dir=.git .
   ```

4. If any marker or suspicious partial resolution remains, repeat semantic resolution from the top and rerun validation.

A conflict is resolved only when the migration engine and generated plans are conceptually coherent and verified, not merely when Git accepts the files.
