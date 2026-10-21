# Vault Example Fixture

## License and Usage

This example contract is licensed under the same terms as the soroban-testkit workspace (see [LICENSE](../../LICENSE) in the repository root).

**This is a test fixture, not production code.**

## Purpose

The `vault` example is a deliberately buggy Soroban contract used in:
- soroban-testkit's documentation
- Integration tests for the CLI
- Examples of testing patterns and error detection

## Key Characteristics

- **Intentionally Buggy**: Contains known issues for testing detection tooling
- **Not For Production**: Never use this contract in production environments
- **Test Fixture Only**: Marked with `publish = false` to prevent crates.io publication
- **Documentation Example**: Demonstrates common contract patterns and testing approaches

## Authorized lifecycle

`deposit` and `withdraw` remain open (and `withdraw` remains deliberately
missing its auth check) so the authorization examples have a real bug to
catch. The `_checked` entry points cover the safe lifecycle:

- `deposit_checked(amount)` requires the owner's authorization, credits the
  balance, and emits a `Deposited { amount }` event.
- `withdraw_checked(amount)` requires the owner's authorization, rejects an
  overdraft with `VaultError::InsufficientBalance`, debits the balance, and
  emits a `Withdrawn { amount }` event.

`tests/lifecycle.rs` asserts balances, events, and the owner-only auth gate
for both entry points.

## Metadata

- **Package**: `vault`
- **Purpose**: Testing and documentation fixture
- **Status**: Intentionally buggy, maintained for testing purposes only
- **License**: Apache-2.0, same as the workspace
