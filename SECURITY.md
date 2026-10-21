# Security Policy

`soroban-testkit` is a testing library and CLI; it is not itself deployed
on-chain. That said, its `audit` subcommand and `AuthMatrix` helper make
security-relevant claims about contracts, so incorrect results matter.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting form:

<https://github.com/soroban-testkit/testkit-blockchain/security/advisories/new>

This is the monitored reporting route. Reports submitted there are visible
only to the repository maintainers, who triage them directly. Do not open a
public issue, discussion, or pull request containing vulnerability details.

If the private form is unavailable, open a public issue titled
`Security contact request` with no vulnerability details and a maintainer
will establish a private channel.

Please include:

- The affected module or CLI command
- A minimal reproduction
- The impact you believe it has (e.g. `audit` missing a real bug,
  `AuthMatrix` reporting a false pass)

We aim to acknowledge reports within 5 business days and to provide a
remediation plan within 10 business days. Please keep the report private
until a fix has been released or we agree on a disclosure date.

## Scope notes

- `soroban-testkit audit` is a linter with a small set of heuristics, not a
  security product. A missed finding is a bug we want to fix, but the
  absence of an audit finding is never a security guarantee.
- The crate has zero non-dev network dependencies; any change that
  introduces network access at test time is treated as a regression.
