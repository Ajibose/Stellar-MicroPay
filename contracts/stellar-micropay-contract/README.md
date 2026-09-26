# Stellar MicroPay — Soroban Contract

This directory contains the Soroban smart contract for Stellar MicroPay.

## Overview

The contract is written in Rust and compiled to WebAssembly (WASM) for deployment on the Stellar network via Soroban.

**Current features (v0.1):**
- Contract initialization with admin
- On-chain tip recording with event emission
- Tip total and count queries per recipient
- Placeholder stubs for escrow and batch payments

## Prerequisites

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Add Soroban WASM target
rustup target add wasm32v1-none

# Install Stellar CLI
cargo install --locked stellar-cli
```

## Build

```bash
stellar contract build --package stellar-micropay-contract --optimize=false
```

The Soroban SDK 28 build uses the `wasm32v1-none` target. The raw release
artifact is written to
`target/wasm32v1-none/release/stellar_micropay_contract.wasm`.

## WASM Size Optimization

Keep the deployed contract WASM below **50 KB**. The release profile in the
workspace root already enables size-oriented optimization, LTO, one codegen
unit, and symbol stripping. Soroban contracts should use `#![no_std]`; avoid
enabling `std` features on guest dependencies unless they are required. Prefer
small dependencies and avoid pulling in unused features. The `testutils` SDK
feature belongs in dev-dependencies only, as it is for this contract.

Install Binaryen to use `wasm-opt`, then build the unoptimized Soroban artifact
and run the size optimizer:

```bash
# Build with Cargo's release profile, without the CLI's wasm-opt pass
stellar contract build --package stellar-micropay-contract --optimize=false

# Apply Binaryen's most aggressive size optimization
wasm-opt -Oz \
  target/wasm32v1-none/release/stellar_micropay_contract.wasm \
  -o target/wasm32v1-none/release/stellar_micropay_contract.opt.wasm

# Print exact byte counts
stat -c '%n: %s bytes' \
  target/wasm32v1-none/release/stellar_micropay_contract.wasm \
  target/wasm32v1-none/release/stellar_micropay_contract.opt.wasm
```

The Stellar CLI build summary also reports the WASM size. To inspect the
contract interface and exported functions, run:

```bash
stellar contract inspect --wasm \
  target/wasm32v1-none/release/stellar_micropay_contract.opt.wasm
```

`contract inspect` prints contract specification details, not the file's byte
size; use `stat` above for the exact size.

Measured using Soroban SDK 28.0.0, Rust 1.93.1, Stellar CLI 28.1.0, and the
workspace release profile:

| Artifact | Size |
| --- | ---: |
| Cargo release WASM, before `wasm-opt` | 9,160 bytes (8.95 KiB) |
| WASM after `wasm-opt -Oz` | 8,006 bytes (7.82 KiB) |

Both are under the 50 KB budget. Re-measure after changing contract code,
dependencies, or compiler/SDK versions; WASM sizes can vary between toolchains.

## Test

```bash
cargo test
```

## Deploy to Testnet

```bash
# Configure your identity
stellar keys generate --global alice --network testnet

# Fund with Friendbot
stellar keys fund alice --network testnet

# Deploy
stellar contract deploy \
  --wasm target/wasm32v1-none/release/stellar_micropay_contract.wasm \
  --source alice \
  --network testnet
```

## Invoke

```bash
# Initialize
stellar contract invoke \
  --id <CONTRACT_ID> \
  --source alice \
  --network testnet \
  -- initialize \
  --admin <YOUR_PUBLIC_KEY>

# Send a tip
stellar contract invoke \
  --id <CONTRACT_ID> \
  --source alice \
  --network testnet \
  -- send_tip \
  --token_address <XLM_SAC_ADDRESS> \
  --from <SENDER_ADDRESS> \
  --to <RECIPIENT_ADDRESS> \
  --amount 1000000

# Check tip total
stellar contract invoke \
  --id <CONTRACT_ID> \
  --network testnet \
  -- get_tip_total \
  --recipient <RECIPIENT_ADDRESS>
```

## Troubleshooting

Soroban SDK 28 requires the Stellar CLI to set build metadata and the
`wasm32v1-none` target on current Rust toolchains. Build with
`stellar contract build` as shown above; a direct `cargo build` or the old
`wasm32-unknown-unknown` target may fail even when the contract source is valid.

## XLM SAC Address (Testnet)

The Stellar Asset Contract address for native XLM on testnet:
```
CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC
```

## Roadmap

- **v2.1** — Escrow payments with time-lock release
- **v2.0** — Batch micro-payment transactions
- **v1.4** — Creator tip pages

See [ROADMAP.md](../../ROADMAP.md) for full details.
