<!--
The PR title must be a conventional commit: it becomes the squash commit and
decides the release. `feat: …` → minor, `fix: …` → patch, `feat!: …` or a
`BREAKING CHANGE:` footer → major. `docs:`, `test:`, `ci:`, `chore:` do not
cut a release.
-->

## What

## Why

## Checks

- [ ] `bash scripts/check.sh` passes
- [ ] Any new or changed `gh` call has a fake-`gh` test
- [ ] README rows for changed commands are current
