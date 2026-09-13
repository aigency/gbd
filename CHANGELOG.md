# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.8.3] - 2026-09-13

### Fixed

- fix(gh): wait out GitHub's primary rate limit; the importer checks its budget before a create (#55)

## [1.8.2] - 2026-09-13

### Fixed

- fix(import): a kind:id blocks target is that relation; a dropped assignee's name stays in the footer (#53)

## [1.8.1] - 2026-09-13

### Documentation

- docs: cargo binstall needs cargo-binstall installed first (#49)

### Fixed

- fix(init): keep gh's scope hint when an org create fails (#51)

## [1.8.0] - 2026-09-13

### Added

- feat(import): keep every Beads relation the export carries (#47)

## [1.7.0] - 2026-09-13

### Added

- feat: Decision, a sixth org issue type (#46)

### Documentation

- docs: getting gbd into a cloud or CI session (#42)

## [1.6.1] - 2026-09-13

### Fixed

- fix(import): map Beads assignees to GitHub logins; a rejected login no longer holds a bead open (#38)

### Other

- ci: an outside pull request is one from a fork, not one by a private member (#43)

## [1.6.0] - 2026-09-13

### Added

- feat(init): shape a new board's default view and put the columns in order (#36)

### Documentation

- docs: retiring Beads is the last migration step (#35)
- docs: export from Beads right before the import, with Dolt pulled and frozen (#32)

## [1.5.0] - 2026-09-12

### Added

- feat: back off on GitHub's secondary rate limit; document migrating from Beads (#26)

## [1.4.0] - 2026-09-12

### Added

- feat: gbd import --from-beads FILE --yes creates the issues, resumably (#25)

## [1.3.0] - 2026-09-12

### Added

- feat: gbd import --from-beads FILE --dry-run (#23)

## [1.2.0] - 2026-09-12

### Added

- feat: keep a Blocked column on the board (#20)

### Other

- ci: one weekly Dependabot PR for all action updates (#8)
- ci: pin every action to a commit SHA (#7)

## [1.1.0] - 2026-09-12

### Added

- feat: signed releases (minisign + build provenance) (#4)

### Documentation

- docs: gbd is on crates.io; list binstall and cargo install (#3)

### Fixed

- fix(release): sign on ubuntu-24.04, where minisign is packaged (#6)
- fix(release): push the release commit with RELEASE_TOKEN (#5)

### Other

- build(deps): bump the cargo-minor group with 5 updates (#1)

## [1.0.0] - 2026-09-12

### Other

- feat!: gbd 1.0.0


