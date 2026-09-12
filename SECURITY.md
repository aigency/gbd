# Security

gbd is a thin client over the GitHub CLI: it never stores credentials, never opens network connections of its own, and only writes to GitHub through `gh` under the permissions of the logged-in account. The interesting surface is therefore what gbd asks `gh` to do, and what it does with `gh`'s output.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private vulnerability reporting on this repository (**Security** → **Report a vulnerability**). You will get an acknowledgement within a week, and a fix or a decision within thirty days of a confirmed report.

## Supported versions

Only the latest release is supported. `gbd --version` prints the version and the commit it was built from.
