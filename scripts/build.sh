#!/bin/bash
set -e

# RemitLend Build Script
# This script builds and optimizes the Soroban smart contracts.

# Navigate to the contracts directory
cd "$(dirname "$0")/../contracts"

echo "Building contracts..."
cargo build --target wasm32-unknown-unknown --release

echo "Contracts built successfully."

# Check if stellar/soroban CLI is available for optimization
if command -v stellar &> /dev/null; then
    echo "Optimizing contracts with stellar CLI..."
    stellar contract optimize
elif command -v soroban &> /dev/null; then
    echo "Optimizing contracts with soroban CLI..."
    soroban contract optimize
else
    echo "WARNING: stellar/soroban CLI not found. Skipping optimization."
    echo "Compiled WASM files are in target/wasm32-unknown-unknown/release/"
fi
