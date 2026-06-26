<!--
  Thanks for contributing to hdl-schemview! Fill in each section below.
  See CONTRIBUTING.md for the full gate list and dev setup.
-->

## Summary

<!-- What does this PR change, and why? Keep it focused on one logical change. -->

## Type of change

<!-- Match the Conventional Commit type of your commits (see CLAUDE.md). Check one. -->

- [ ] `feat` — new feature for the user
- [ ] `fix` — bug fix for the user
- [ ] `docs` — documentation only
- [ ] `style` — formatting / no production-code change
- [ ] `refactor` — production-code refactor, no behavior change
- [ ] `test` — adding or refactoring tests
- [ ] `chore` — build, tooling, deps

## Related issues

<!-- e.g. Closes #123 -->

## How tested

<!-- Commands run, fixtures used, manual steps. -->

## Checklist

<!-- Tick what applies. Leave unticked rows that don't apply to this change. -->

- [ ] **Rust** — `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all` pass (from `core/`).
- [ ] **Matcher gate** — if matcher/model changed, the cross-probe hit-rate is still ≥ 95% (`cargo run --bin svxprobe -- match …`).
- [ ] **Frontend** — if `app/` changed, `npm run build` and `npm test` pass (from `app/`).
- [ ] **Python** — if `elaborate/` changed, `uv run pytest -q` passes (from `elaborate/`).
- [ ] **DTO sync** — if `gui`/`schematic` serde DTOs changed, `app/src/types.ts` is updated to match.
- [ ] No reintroduced heuristics / string-matching where a model lookup exists (single-source-of-truth principle).
- [ ] Commit messages follow Conventional Commits.
