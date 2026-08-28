# Security Policy

## Supported versions

Security fixes are applied to the default branch and the latest published release. Older releases may require upgrading before a fix can be applied safely.

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability.

Use GitHub's private vulnerability reporting flow from the repository's **Security** tab when it is available. Include:

- the affected command, migration input, and database flavor;
- the smallest reproduction that demonstrates the issue;
- the expected and observed behavior;
- whether credentials, schema contents, or destructive operations may be exposed;
- any suggested remediation or test case.

Remove production credentials and sensitive schema data from the report. Maintainers should acknowledge a complete report, reproduce it privately, prepare regression coverage, and coordinate disclosure with the reporter before publishing details.
