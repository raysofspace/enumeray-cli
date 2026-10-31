# Feature development

Follow the [feature workflow](CONTRIBUTING.md#feature-development) and
[verification guide](CONTRIBUTING.md#build-and-local-checks) for all changes.

- Preserve existing work and create a `codex/<feature>` branch from the actual
  target branch. Never silently substitute a missing comparison ref. Use a
  separate workspace for each concurrent feature.
- This repository owns the public CLI, client, connector, contracts, guides,
  fixtures and packaging. Keep sources canonical and add focused regressions
  for changed behavior. Regenerate affected derived documentation where consumed.
- Preview checks with `scripts/verify.sh plan --area <profile>` and run the
  affected profile and required backend fixtures. Register new suites and ignored
  tests in `scripts/test-strategy.json` and run `scripts/verify.sh validate` after
  changing suite structure. Preserve deterministic race, authorization,
  cancellation, rollback and recovery coverage; large-scale qualification remains
  explicitly selected.
- For coordinated service work, commit and push public changes first, link the
  related PRs and merge this PR first. The service must pin the final merged
  public SHA after any squash/rebase and verify the final source pair before its
  PR merges. Public-only changes can be adopted by the service later.
- After merging, preserve remaining work, fast-forward local `main`, delete
  confirmed merged feature branches locally and remotely, and archive temporary
  managed worktrees after preserving their work. When working as a submodule,
  finish at the parent repository's recorded pin, even if public `main` is newer.
  Check for leftovers without discarding unrelated edits or retained evidence.

Source tests do not qualify a distributed release. Publication separately
requires the selected service/public revisions and exact final artifacts to pass
their qualification gates. Keep local measurements separate from CI evidence;
do not dispatch hosted workflows or push solely for measurements while hosted
billing is blocked.
