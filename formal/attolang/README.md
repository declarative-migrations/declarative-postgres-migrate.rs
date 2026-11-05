# DPM cross-dialect migration planning and apply fence: bounded formal model and draft AttoLang oracle

Run `node formal/attolang/model_check.mjs`. The checker explores the
entire finite state graph and requires positive and adversarial witnesses.
A deliberately unsafe transition must be rejected (negative control).

The `.atto` module is a source-level pure policy oracle following the draft
AttoLang syntax. **Neither this Node checker nor this CI workflow compiles
or executes AttoLang, drives the production implementation, or proves
cross-backend parity.** This is abstract model proof only.

The finite model checks matched vs mismatched dialects and basic phase ordering only. It is not a proof of SQL schema convergence, transactional DDL behavior, server catalog semantics, or production enforcement by dpm.

Promotion requires production trace/refinement replay against exact-head
code with nonzero executed steps and compiler-produced `.atto` guests
independently checked on Java, LLVM, Wasm, Forge and .NET.
