# Feature development

Follow the [feature workflow](CONTRIBUTING.md#feature-development) and
[verification guide](CONTRIBUTING.md#build-and-local-checks) for all changes.

- In the PR flow, commits and pushes may proceed within the task's scope without
  separate confirmation. In the explicit `main` flow, do not commit or push until
  the user explicitly authorizes those actions; leave changes uncommitted for
  review. A request to work on `main` alone does not authorize commits or pushes.
  Respect any separate limits on committing and pushing.
- Preserve existing work. By default, create a `feature/<name>` branch from the
  actual target branch. Never silently substitute a missing comparison ref. Use
  a separate workspace for each concurrent feature.
- When the user explicitly requests work on `main`, use the existing main
  checkout and do not create feature branches, PRs or new worktrees for that task.
  Fast-forward main before editing and record the starting SHA. If direct pushes
  are blocked, report the constraint rather than creating a PR or force-pushing
  `main`. Verification and source ownership still apply.
- This repository owns the public CLI, client, connector, contracts, guides,
  fixtures and packaging. Keep sources canonical and add focused regressions
  for changed behavior. Regenerate affected derived documentation where consumed.
- Preview checks with `scripts/verify.sh plan --area <profile>` and run the
  affected profile and required backend fixtures. Register new suites and ignored
  tests in `scripts/test-strategy.json` and run `scripts/verify.sh validate` after
  changing suite structure. Preserve deterministic race, authorization,
  cancellation, rollback and recovery coverage; large-scale qualification remains
  explicitly selected.
- For coordinated service work, once authorized, commit public changes before
  the service records their SHA and push public changes before the service push.
  In the default PR flow, link the PRs, merge this PR first and have the service
  pin the final merged public SHA after any squash/rebase. In the explicit `main`
  flow, the service pins the exact adopted public commit. Verify the final source
  pair before the service PR merges or its main update is pushed. Public-only
  changes can be adopted by the service later.
- After merging or authorized direct pushes, preserve remaining work and
  fast-forward local `main`. After each PR merge, remove its merged feature branch
  both locally and remotely, after confirming it contains no later unmerged work.
  Archive temporary managed worktrees after preserving their work; skip branch/PR
  cleanup in the explicit `main` flow unless the user separately requests it.
  When working as a submodule, finish at the parent repository's recorded pin,
  even if public `main` is newer.
  Check for leftovers without discarding unrelated edits or retained evidence.

Source tests do not qualify a distributed release. Publication separately
requires the selected service/public revisions and exact final artifacts to pass
their qualification gates. Keep local measurements separate from CI evidence;
do not dispatch hosted workflows or push solely for measurements while hosted
billing is blocked.
