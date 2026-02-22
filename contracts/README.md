# RemitLend Contracts

This project uses [Soroban](https://soroban.stellar.org/), the smart contract platform on Stellar.

## Prerequisites

- [Rust Toolchain](https://www.rust-lang.org/tools/install)
- [Soroban CLI](https://soroban.stellar.org/docs/getting-started/setup)

## Setup

To initialize the project structure (if not already done):

```bash
cargo new --lib contracts
```

## Structure

- `contracts/`: Contains the Rust smart contracts.
- `contracts/src/`: Source code.
- `lending_pool/`: Lending pool contract for deposits and withdrawals.
- `loan_manager/`: Loan management contract for handling loan requests and repayments.
- `remittance_nft/`: NFT contract for user reputation scores and history.
- `fuzz/`: Fuzz testing setup and targets.

## Build

```bash
cargo build --target wasm32-unknown-unknown --release
```

## Testing

### Unit Tests

```bash
cargo test
```

### Fuzz Testing

This project includes comprehensive fuzz testing to find edge cases and potential vulnerabilities. See [FUZZING_README.md](FUZZING_README.md) for detailed setup and usage instructions.

#### Quick Start

```bash
# Install fuzzing tools
cargo install cargo-fuzz cargo-test-fuzz

# Run fuzz campaign
cd contracts
./fuzz_campaign.sh

# Or run individual fuzz tests
cd lending_pool
cargo test-fuzz --test test_deposit_withdraw_invariants --run-until-crash
```

#### Fuzzing Coverage

- **LendingPool**: Deposit/withdrawal invariants, balance consistency
- **LoanManager**: Score thresholds, authorization controls
- **RemittanceNFT**: Unique NFT enforcement, metadata integrity

## Contracts Overview

### LendingPool

Manages token deposits and withdrawals with the following key features:

- Secure deposit and withdrawal operations
- Individual balance tracking
- Authorization controls

### LoanManager

Handles loan requests and repayments:

- Score-based loan eligibility (minimum 500 points)
- Integration with RemittanceNFT for score management
- Repayment processing

### RemittanceNFT

Manages user reputation through NFTs:

- Unique NFT per user
- Score tracking and updates
- History hash management
- Backward compatibility with legacy score data

## Security

The contracts include comprehensive security measures:

- Authorization checks on all sensitive operations
- Input validation and bounds checking
- Fuzz testing for edge case detection
- Invariant verification through property-based testing

For detailed security considerations and fuzz testing results, see [FUZZING_README.md](FUZZING_README.md).
