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

# Add WASM target
rustup target add wasm32v1-none

# Install Stellar CLI
cargo install --locked stellar-cli
```

## Build

```bash
stellar contract build
```

Output: `target/wasm32v1-none/release/stellar_micropay_contract.wasm`
(relative to the workspace root, not this directory)

## Test

```bash
cargo test
```

## Security review (#1121)

Every entry point in `src/lib.rs` was reviewed for authorization correctness,
and the outcome is documented as a doc comment above each function.

| Entry point | Authorization | Review note |
| --- | --- | --- |
| `initialize` | `admin` | Was missing — see finding 1 |
| `send_tip` | `from` | Auth covers `(token_address, from, to, amount)`; amount validated first |
| `mint_receipt` | `from` | Auth covers `(from, to, amount, memo)`; records keyed by the authenticated payer |
| `get_tip_total`, `get_tip_count`, `get_admin`, `get_tip_record`, `get_receipt_count`, `get_receipt` | none (read-only) | No state is written and the values are public |
| `create_escrow`, `batch_send` | none (stubs) | Always panic before touching state |

### Findings fixed

1. **`initialize` stored an admin without authorization (critical).** It wrote
   `DataKey::Admin` without any auth check, so any account could install an
   arbitrary admin. Fixed by calling `admin.require_auth()` before the write.
2. **`require_auth` ran before argument validation (low)** in `send_tip` and
   `mint_receipt`. A panicking sub-call does not roll back sibling effects in
   the same transaction when the contract is invoked by another contract, so a
   rejected call could leave a satisfied auth entry behind for the payer.
   Fixed by validating `amount` before requesting authorization.

### Residual risk (documented, not changed)

- `initialize` is a separate invocation from the deploy, so the first caller
  can still install *themselves* as admin: `require_auth` proves the stored
  admin consented to the role, it cannot prove that address is the deployer.
  Deploy through a `__constructor` (deploy-time initialization) or invoke
  `initialize` in the same transaction as the deploy to remove that window.
- Confirmed already correct: the nested SAC `transfer` in `send_tip` sits under
  the sender's authorization; auth arguments match invocation arguments, so
  argument swapping is rejected by the host; `mint_receipt` writes are keyed by
  the authenticated payer; the getters are read-only; the escrow and batch
  stubs always panic.

### Auth test coverage

`cargo test` exercises every `require_auth` call site, including the negative
cases that must fail:

- `test_initialize_requires_admin_auth` — the admin is the sole authorizer.
- `test_initialize_rejects_unauthorized_admin` — no authorization at all fails,
  and the contract stays uninitialized.
- `test_initialize_rejects_authorization_of_another_address` — authorizing a
  different address cannot install another account as admin.
- `test_send_tip_moves_funds_and_requires_from_auth` / `test_send_tip_rejects_unauthorized_caller`
  — funds move under the sender's authorization, and an unauthorized caller
  moves nothing.
- `test_mint_receipt_requires_from_auth` / `test_mint_receipt_rejects_unauthorized_caller`
  — receipts are minted only under the payer's authorization.
- `test_failed_mint_does_not_consume_auth` — a rejected call consumes no
  authorization and leaves no state behind.

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

## Troubleshooting (#153)

The CLI commands above only work if the contract compiles — and as of this
writing `src/lib.rs` carries unresolved merge residue that blocks
`cargo build`:

- ~~Two `DataKey` enums were defined at module scope.~~ Merged into one in
  this PR — both sets of variants are needed by the contract methods.
- `impl MicroPayContract { ... }` should be `impl StellarMicroPay`. The
  `initialize` function lost its signature in the same merge — its body
  starts directly after the section comment. A standalone follow-up issue
  needs to reconstruct the function signatures by walking the original
  PRs (`git log -p src/lib.rs`).
- Several other methods (`send_tip`, `close_stream`, etc.) appear to have
  bodies that reference identifiers from neighboring functions, suggesting
  more than one merge dropped function boundaries.

If `cargo build --target wasm32-unknown-unknown --release` fails with
"unexpected closing delimiter" or "cannot find type", check `git blame`
around the offending line first — most of the breakage looks like
incomplete merge resolutions, not real logic bugs. Until the contract
compiles, `stellar contract deploy` has no `.wasm` artifact to upload, so
every CLI step from "Deploy to Testnet" onward is blocked.

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
