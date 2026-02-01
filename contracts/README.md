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

## Build

```bash
cargo build --target wasm32-unknown-unknown --release
```
