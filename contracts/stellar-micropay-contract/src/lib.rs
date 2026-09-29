#![no_std]

/**
 * contracts/stellar-micropay-contract/src/lib.rs
 *
 * Stellar MicroPay — Soroban Smart Contract
 *
 * Provides:
 *   - Escrow payments (ROADMAP v2.1)
 *   - Creator tipping (ROADMAP v1.4)
 *   - Micro-transaction batching (ROADMAP v2.0)
 *   - NFT payment receipts (ROADMAP v1.5)
 *
 * Build:
 *   cargo build --target wasm32-unknown-unknown --release
 *
 * Deploy (Stellar CLI):
 *   stellar contract deploy \
 *     --wasm target/wasm32-unknown-unknown/release/stellar_micropay_contract.wasm \
 *     --source YOUR_SECRET_KEY \
 *     --network testnet
 */

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    token, Address, Env, Symbol, Vec,
};

// ─── Data types ───────────────────────────────────────────────────────────────

/// A single tip event recorded on-chain.
#[contracttype]
#[derive(Clone, Debug)]
pub struct TipRecord {
    /// The sender's Stellar address
    pub from: Address,
    /// The recipient's Stellar address
    pub to: Address,
    /// Amount in stroops (1 XLM = 10_000_000 stroops)
    pub amount: i128,
    /// Ledger number when this tip was sent
    pub ledger: u32,
}

/// On-chain receipt metadata minted as proof of payment.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ReceiptMetadata {
    /// The payer's Stellar address
    pub from: Address,
    /// The payee's Stellar address
    pub to: Address,
    /// Amount in stroops (1 XLM = 10_000_000 stroops)
    pub amount: i128,
    /// ISO-8601 timestamp of when the receipt was minted
    pub timestamp: u64,
    /// Optional payment memo
    pub memo: Symbol,
    /// Ledger number when this receipt was minted
    pub ledger: u32,
}

/// A payment stream: a locked deposit that accrues to one recipient at a
/// fixed rate per ledger.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    /// The payer's Stellar address (the stream owner)
    pub payer: Address,
    /// The recipient entitled to claim the accrued stream
    pub recipient: Address,
    /// Accrual rate in the token's smallest unit, per ledger
    pub rate_per_ledger: i128,
    /// Total amount escrowed for this stream
    pub deposited: i128,
    /// Amount already withdrawn by the recipient
    pub claimed: i128,
    /// Ledger number when the stream began accruing
    pub start_ledger: u32,
    /// The token escrowed at open time; claims and refunds are paid from it
    pub token: Address,
}

/// The kind of state transition recorded in a stream's event log.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEventType {
    /// Recipient withdrew accrued funds
    Claim,
    /// Payer added funds to the deposit
    TopUp,
    /// Stream was closed and remaining funds refunded
    Close,
}

/// One entry in a stream's append-only event log.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct StreamEvent {
    /// What kind of transition produced this entry
    pub event_type: StreamEventType,
    /// Amount involved, in the token's smallest unit
    pub amount: i128,
    /// Ledger number when the entry was appended
    pub ledger: u32,
}

/// Errors returned by the contract.
#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ContractError {
    /// Stream does not exist
    StreamNotFound = 1,
    /// The rate exceeds the admin-configured maximum
    RateTooHigh = 2,
    /// The contract is frozen; state-changing calls are rejected
    Frozen = 3,
    /// `rate_per_ledger` must be positive
    InvalidRate = 4,
    /// The deposit must be positive
    InvalidDeposit = 5,
    /// The caller is not this stream's recipient
    NotRecipient = 6,
    /// The caller is not this stream's payer
    NotPayer = 7,
    /// The stream has already been closed
    StreamClosed = 8,
    /// The caller is not the contract admin
    NotAdmin = 9,
    /// Nothing has accrued since the last claim
    NothingToClaim = 10,
}

/// Storage key for per-recipient tip totals
#[contracttype]
pub enum DataKey {
    Admin,
    TipTotal(Address),
    TipCount(Address),
    /// Latest tip record for a recipient (indexed by recipient + count)
    TipRecord(Address, u32),
    /// Total receipt count for a payer
    ReceiptCount(Address),
    /// Receipt record indexed by (payer, index)
    ReceiptRecord(Address, u32),
    /// Admin-configured cap on `rate_per_ledger`; `0` disables the cap
    MaxRate,
    /// Emergency pause flag; `true` blocks all state-changing calls
    Frozen,
    /// Number of streams ever opened; also the next stream id
    StreamCount,
    /// Stream record by id
    Stream(u32),
    /// Append-only event log for a stream
    StreamEvents(u32),
}

// ─── Guards ───────────────────────────────────────────────────────────────────

/// Load the contract admin, panicking if the contract is uninitialized.
fn load_admin(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .expect("Contract not initialized")
}

/// Require `caller` to be the admin, and require its authorization.
///
/// Panics with `ContractError::NotAdmin` when the address is not the stored
/// admin. The auth check happens only after the identity comparison, so a
/// non-admin caller cannot burn its own authorization on a doomed call.
fn require_admin(env: &Env, caller: &Address) {
    if load_admin(env) != *caller {
        panic_with_error(env, ContractError::NotAdmin);
    }
    caller.require_auth();
}

/// Panic with a typed `ContractError`.
///
/// Soroban's generated client surfaces `ContractError` discriminants, which
/// makes `try_*` assertions in tests stable across message wording changes.
fn panic_with_error(env: &Env, err: ContractError) -> ! {
    env.panic_with_error(&err)
}

/// Reject the call if the contract is frozen.
///
/// Read-only getters deliberately do **not** call this: a frozen contract must
/// stay queryable so integrators can still inspect stream state during an
/// incident.
fn require_not_frozen(env: &Env) {
    let frozen: bool = env
        .storage()
        .instance()
        .get(&DataKey::Frozen)
        .unwrap_or(false);
    if frozen {
        panic_with_error(env, ContractError::Frozen);
    }
}

/// The admin-configured cap on `rate_per_ledger`. `0` (the default) disables
/// the cap entirely.
fn load_max_rate(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::MaxRate)
        .unwrap_or(0)
}

/// Reject `rate_per_ledger` when it exceeds the admin cap.
///
/// A cap of `0` means "no cap", so it is never itself exceeded.
fn check_rate_cap(env: &Env, rate_per_ledger: i128) {
    let max_rate = load_max_rate(env);
    if max_rate != 0 && rate_per_ledger > max_rate {
        panic_with_error(env, ContractError::RateTooHigh);
    }
}

fn load_stream(env: &Env, stream_id: u32) -> Stream {
    env.storage()
        .instance()
        .get(&DataKey::Stream(stream_id))
        .unwrap_or_else(|| panic_with_error(env, ContractError::StreamNotFound))
}

fn save_stream(env: &Env, stream_id: u32, stream: &Stream) {
    env.storage()
        .instance()
        .set(&DataKey::Stream(stream_id), stream);
}

/// Append an entry to a stream's persistent event log.
fn append_stream_event(env: &Env, stream_id: u32, event_type: StreamEventType, amount: i128) {
    let mut events: Vec<StreamEvent> = env
        .storage()
        .persistent()
        .get(&DataKey::StreamEvents(stream_id))
        .unwrap_or_else(|| Vec::new(env));

    events.push_back(StreamEvent {
        event_type,
        amount,
        ledger: env.ledger().sequence(),
    });

    env.storage()
        .persistent()
        .set(&DataKey::StreamEvents(stream_id), &events);
}

/// Total amount that has accrued to the recipient as of `current_ledger`.
///
/// The accrual window is capped at the ledger where the deposit runs out, so
/// the result is structurally bounded by `deposited` and can neither exceed it
/// nor overflow `i128` — no post-hoc clamp needed.
fn total_streamed_amount(stream: &Stream, current_ledger: u32) -> i128 {
    let elapsed_ledgers = current_ledger.saturating_sub(stream.start_ledger);

    // Integer division, so this is 0 for a rate larger than the whole deposit.
    let funded_ledgers = (stream.deposited / stream.rate_per_ledger) as u64;

    // Shadow `elapsed_ledgers` with the funded window. Capping before the
    // multiply is what makes the plain `*` below safe: the product is bounded
    // by `deposited`, so it can neither overflow i128 nor exceed the escrow.
    // (An uncapped `rate * elapsed` would overflow for large inputs, which is
    // exactly what `fuzz_claim_math_holds_for_arbitrary_inputs` pins down.)
    let elapsed_ledgers: u32 = if u64::from(elapsed_ledgers) < funded_ledgers {
        elapsed_ledgers
    } else if funded_ledgers > u64::from(u32::MAX) {
        u32::MAX
    } else {
        funded_ledgers as u32
    };

    stream.rate_per_ledger * elapsed_ledgers as i128
}

/// Amount the recipient can withdraw right now, always in `[0, deposited - claimed]`,
/// so repeated claims can never withdraw more than the escrow holds.
///
/// The early return matters: `i128::saturating_sub` saturates at `i128::MIN`,
/// not at 0, so an over-claimed stream would otherwise produce a large negative
/// "remainder" rather than the correct 0.
fn claimable_amount(stream: &Stream, current_ledger: u32) -> i128 {
    if stream.claimed >= stream.deposited {
        return 0;
    }

    let total_streamed = total_streamed_amount(stream, current_ledger);

    // `saturating_sub`, not a plain `-`: a recipient that claims before the
    // accrual catches up (a top-up does not retroactively increase what has
    // already streamed) sits above `total_streamed`, and a plain subtract would
    // underflow and abort the contract. `.max(0)` then reports 0 rather than a
    // negative amount.
    let claimable = total_streamed.saturating_sub(stream.claimed).max(0);

    // Structural bound: never offer more than the deposit still holds. Past the
    // early return above, `claimed < deposited`, so this cannot go negative.
    let remaining = stream.deposited - stream.claimed;
    if claimable < remaining {
        claimable
    } else {
        remaining
    }
}

// ─── Contract ─────────────────────────────────────────────────────────────────

#[contract]
pub struct MicroPayContract;

#[contractimpl]
impl MicroPayContract {

    // ─── Initialization ──────────────────────────────────────────────────────

    /// Initialize the contract with an admin address.
    /// Can only be called once.
    pub fn initialize(env: Env, admin: Address) {
        // Ensure not already initialized
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("Contract already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
    }

    // ─── Administration ─────────────────────────────────────────────────────

    /// Set the maximum permitted `rate_per_ledger` for new streams.
    ///
    /// Without a cap a payer can set an arbitrarily high rate that drains the
    /// whole deposit in a single ledger. Setting `max_rate` to `0` disables the
    /// cap, which is the default and the pre-cap behavior.
    ///
    /// Only the contract admin may call this. Lowering the cap does not affect
    /// streams that already exist.
    pub fn set_max_rate(env: Env, admin: Address, max_rate: i128) {
        require_not_frozen(&env);
        require_admin(&env, &admin);

        if max_rate < 0 {
            panic_with_error(&env, ContractError::InvalidRate);
        }

        env.storage().instance().set(&DataKey::MaxRate, &max_rate);

        env.events().publish(
            (Symbol::new(&env, "max_rate"), admin),
            max_rate,
        );
    }

    /// Get the current maximum permitted `rate_per_ledger`. `0` means uncapped.
    pub fn get_max_rate(env: Env) -> i128 {
        load_max_rate(&env)
    }

    /// Pause all state-changing operations until `unfreeze` is called.
    ///
    /// Intended for incident response: a bug in a stream or tip path can be
    /// halted without redeploying. Read-only getters keep working so integrators
    /// and monitoring can still inspect state while frozen.
    pub fn freeze(env: Env, admin: Address) {
        require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::Frozen, &true);
        env.events().publish((Symbol::new(&env, "freeze"), admin), true);
    }

    /// Clear the freeze flag, re-enabling state-changing operations.
    pub fn unfreeze(env: Env, admin: Address) {
        require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::Frozen, &false);
        env.events().publish((Symbol::new(&env, "unfreeze"), admin), false);
    }

    /// Whether the contract is currently frozen.
    pub fn is_frozen(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Frozen)
            .unwrap_or(false)
    }

    // ─── Streaming payments ─────────────────────────────────────────────────

    /// Open a payment stream, escrowing `deposit` and releasing it to
    /// `recipient` at `rate_per_ledger` per ledger.
    ///
    /// Returns the new stream id. `rate_per_ledger` must be positive and must
    /// not exceed the admin-configured cap.
    pub fn open_stream(
        env: Env,
        token_address: Address,
        payer: Address,
        recipient: Address,
        rate_per_ledger: i128,
        deposit: i128,
    ) -> u32 {
        require_not_frozen(&env);
        payer.require_auth();

        if rate_per_ledger <= 0 {
            panic_with_error(&env, ContractError::InvalidRate);
        }
        check_rate_cap(&env, rate_per_ledger);
        if deposit <= 0 {
            panic_with_error(&env, ContractError::InvalidDeposit);
        }

        // Escrow the deposit; claims and refunds are paid from here.
        let token = token::Client::new(&env, &token_address);
        token.transfer(&payer, &env.current_contract_address(), &deposit);

        let stream_id: u32 = env
            .storage()
            .instance()
            .get(&DataKey::StreamCount)
            .unwrap_or(0);

        let stream = Stream {
            payer,
            recipient,
            rate_per_ledger,
            deposited: deposit,
            claimed: 0,
            start_ledger: env.ledger().sequence(),
            token: token_address,
        };
        save_stream(&env, stream_id, &stream);
        env.storage()
            .instance()
            .set(&DataKey::StreamCount, &(stream_id + 1));

        env.events().publish(
            (Symbol::new(&env, "stream_open"), stream_id),
            (rate_per_ledger, deposit),
        );

        stream_id
    }

    /// Withdraw everything accrued so far. Returns the amount transferred.
    ///
    /// The result is capped at the remaining deposit, so repeated claims can
    /// never withdraw more than `deposited` in total.
    pub fn claim_stream(env: Env, stream_id: u32, recipient: Address) -> i128 {
        require_not_frozen(&env);
        recipient.require_auth();

        let mut stream = load_stream(&env, stream_id);
        if stream.recipient != recipient {
            panic_with_error(&env, ContractError::NotRecipient);
        }
        if stream.claimed >= stream.deposited {
            panic_with_error(&env, ContractError::StreamClosed);
        }

        let amount = claimable_amount(&stream, env.ledger().sequence());
        if amount <= 0 {
            panic_with_error(&env, ContractError::NothingToClaim);
        }

        stream.claimed = stream.claimed.saturating_add(amount);
        save_stream(&env, stream_id, &stream);

        append_stream_event(&env, stream_id, StreamEventType::Claim, amount);

        let token = token::Client::new(&env, &stream.token);
        token.transfer(&env.current_contract_address(), &stream.recipient, &amount);

        env.events().publish(
            (Symbol::new(&env, "stream_claim"), stream_id),
            amount,
        );

        amount
    }

    /// Add `amount` to the stream's deposit, extending its duration.
    pub fn top_up_stream(env: Env, stream_id: u32, payer: Address, amount: i128) {
        require_not_frozen(&env);
        payer.require_auth();

        if amount <= 0 {
            panic_with_error(&env, ContractError::InvalidDeposit);
        }

        let mut stream = load_stream(&env, stream_id);
        if stream.payer != payer {
            panic_with_error(&env, ContractError::NotPayer);
        }

        let token = token::Client::new(&env, &stream.token);
        token.transfer(&payer, &env.current_contract_address(), &amount);

        stream.deposited = stream.deposited.saturating_add(amount);
        save_stream(&env, stream_id, &stream);

        append_stream_event(&env, stream_id, StreamEventType::TopUp, amount);

        env.events().publish(
            (Symbol::new(&env, "stream_topup"), stream_id),
            amount,
        );
    }

    /// Close the stream and refund whatever the recipient has not claimed.
    pub fn close_stream(env: Env, stream_id: u32, payer: Address) -> i128 {
        require_not_frozen(&env);
        payer.require_auth();

        let stream = load_stream(&env, stream_id);
        if stream.payer != payer {
            panic_with_error(&env, ContractError::NotPayer);
        }

        let refund = stream.deposited.saturating_sub(stream.claimed);

        let token = token::Client::new(&env, &stream.token);
        token.transfer(&env.current_contract_address(), &stream.payer, &refund);

        append_stream_event(&env, stream_id, StreamEventType::Close, refund);

        env.events().publish(
            (Symbol::new(&env, "stream_close"), stream_id),
            refund,
        );

        refund
    }

    /// Get a stream record by id. Works while frozen.
    pub fn get_stream(env: Env, stream_id: u32) -> Stream {
        load_stream(&env, stream_id)
    }

    /// Get the amount currently claimable from a stream. Works while frozen.
    pub fn get_claimable(env: Env, stream_id: u32) -> i128 {
        let stream = load_stream(&env, stream_id);
        claimable_amount(&stream, env.ledger().sequence())
    }

    /// Get the full append-only event log for a stream.
    ///
    /// Entries are in chronological order. Works while frozen.
    pub fn get_stream_history(env: Env, stream_id: u32) -> Vec<StreamEvent> {
        env.storage()
            .persistent()
            .get(&DataKey::StreamEvents(stream_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get the number of streams ever opened.
    pub fn get_stream_count(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::StreamCount)
            .unwrap_or(0)
    }

    // ─── Tipping ─────────────────────────────────────────────────────────────

    /// Send a tip from `from` to `to` using a Stellar token.
    ///
    /// Parameters:
    ///   - token_address: The SAC (Stellar Asset Contract) address for the token (e.g. XLM)
    ///   - from:          The sender (must authorize this call)
    ///   - to:            The recipient
    ///   - amount:        Amount in the token's smallest unit (stroops for XLM)
    ///
    /// This records the tip on-chain for analytics and emits an event.
    pub fn send_tip(
        env: Env,
        token_address: Address,
        from: Address,
        to: Address,
        amount: i128,
    ) {
        require_not_frozen(&env);

        // Require sender authorization
        from.require_auth();

        // Validate amount
        if amount <= 0 {
            panic!("Tip amount must be positive");
        }

        // Transfer tokens via the Stellar token interface (SAC)
        let token = token::Client::new(&env, &token_address);
        token.transfer(&from, &to, &amount);

        // Update on-chain tip totals for the recipient
        let current_total: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TipTotal(to.clone()))
            .unwrap_or(0);

        let current_count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TipCount(to.clone()))
            .unwrap_or(0);

        env.storage()
            .instance()
            .set(&DataKey::TipTotal(to.clone()), &(current_total + amount));

        env.storage()
            .instance()
            .set(&DataKey::TipCount(to.clone()), &(current_count + 1));

        // Store the tip record so it can be queried later
        let record = TipRecord {
            from: from.clone(),
            to: to.clone(),
            amount,
            ledger: env.ledger().sequence(),
        };
        env.storage()
            .instance()
            .set(&DataKey::TipRecord(to.clone(), current_count), &record);

        // Emit an event for indexers
        env.events().publish(
            (Symbol::new(&env, "tip"), from, to.clone()),
            amount,
        );
    }

    // ─── Getters ─────────────────────────────────────────────────────────────

    /// Get the total amount tipped to a recipient (in stroops).
    pub fn get_tip_total(env: Env, recipient: Address) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::TipTotal(recipient))
            .unwrap_or(0)
    }

    /// Get the number of tips received by a recipient.
    pub fn get_tip_count(env: Env, recipient: Address) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::TipCount(recipient))
            .unwrap_or(0)
    }

    /// Get the contract admin address.
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("Contract not initialized")
    }

    /// Get a specific tip record for a recipient by index.
    pub fn get_tip_record(env: Env, recipient: Address, index: u32) -> TipRecord {
        env.storage()
            .instance()
            .get(&DataKey::TipRecord(recipient, index))
            .expect("Tip record not found")
    }

    // ─── NFT Receipts ───────────────────────────────────────────────────────

    /// Mint an on-chain receipt as proof of payment.
    ///
    /// Stores receipt metadata (amount, timestamp, memo) under the payer's
    /// address and emits a `receipt` event. The returned `u32` is the receipt
    /// index (NFT ID) for this payer.
    ///
    /// Parameters:
    ///   - from:   The payer (must authorize this call)
    ///   - to:     The payee
    ///   - amount: Amount in stroops
    ///   - memo:   Optional payment memo (max 28 chars, passed as a Symbol)
    pub fn mint_receipt(
        env: Env,
        from: Address,
        to: Address,
        amount: i128,
        memo: Symbol,
    ) -> u32 {
        require_not_frozen(&env);

        from.require_auth();

        if amount <= 0 {
            panic!("Receipt amount must be positive");
        }

        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::ReceiptCount(from.clone()))
            .unwrap_or(0);

        let receipt = ReceiptMetadata {
            from: from.clone(),
            to,
            amount,
            timestamp: env.ledger().timestamp(),
            memo,
            ledger: env.ledger().sequence(),
        };

        env.storage()
            .instance()
            .set(&DataKey::ReceiptRecord(from.clone(), count), &receipt);

        env.storage()
            .instance()
            .set(&DataKey::ReceiptCount(from.clone()), &(count + 1));

        env.events().publish(
            (Symbol::new(&env, "receipt"), from),
            count,
        );

        count
    }

    /// Get the total number of receipts minted for a payer.
    pub fn get_receipt_count(env: Env, payer: Address) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::ReceiptCount(payer))
            .unwrap_or(0)
    }

    /// Get a specific receipt for a payer by index.
    pub fn get_receipt(env: Env, payer: Address, index: u32) -> ReceiptMetadata {
        env.storage()
            .instance()
            .get(&DataKey::ReceiptRecord(payer, index))
            .expect("Receipt not found")
    }

    // ─── Placeholders (future features) ──────────────────────────────────────

    /// [PLACEHOLDER] Create an escrow payment that releases after a time lock.
    /// See ROADMAP.md v2.1 — Soroban Escrow Payments.
    ///
    /// Future implementation:
    ///   - Lock funds in the contract
    ///   - Release to recipient after `release_ledger`
    ///   - Allow sender to cancel before release
    pub fn create_escrow(
        env: Env,
        _from: Address,
        _to: Address,
        _amount: i128,
        _release_ledger: u32,
    ) {
        require_not_frozen(&env);
        panic!("Escrow payments coming in v2.1 — see ROADMAP.md");
    }

    /// [PLACEHOLDER] Batch multiple micro-payments in a single transaction.
    /// See ROADMAP.md v2.0 — Multi-Currency Payments.
    pub fn batch_send(
        env: Env,
        _from: Address,
        _recipients: soroban_sdk::Vec<Address>,
        _amounts: soroban_sdk::Vec<i128>,
    ) {
        require_not_frozen(&env);
        panic!("Batch payments coming in v2.0 — see ROADMAP.md");
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use soroban_sdk::{
        testutils::{Address as _, Ledger, MockAuth, MockAuthInvoke},
        token, Address, Env, IntoVal,
    };

    /// Token balance granted to the funded payer.
    const TEST_BALANCE: i128 = 1_000_000_000_000;

    /// Register the contract, initialize it, and return
    /// `(env, client, admin, token_address, payer)` where `payer` holds a
    /// funded token balance ready to escrow.
    fn setup() -> (Env, MicroPayContractClient<'static>, Address, Address, Address) {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&admin);

        let token_admin = Address::generate(&env);
        let token_address = env
            .register_stellar_asset_contract_v2(token_admin.clone())
            .address();

        // Mint before enabling blanket auth mocking: the SAC `mint` needs a
        // real admin signature, and `mock_all_auths` would otherwise be
        // registered too late to cover this call.
        env.mock_auths(&[MockAuth {
            address: &token_admin,
            invoke: &MockAuthInvoke {
                contract: &token_address,
                fn_name: "mint",
                sub_invokes: &[],
                args: (token_admin.clone(), TEST_BALANCE).into_val(&env),
            },
        }]);
        let token_admin_client = token::StellarAssetClient::new(&env, &token_address);
        token_admin_client.mint(&token_admin, &TEST_BALANCE);

        // From here on any authorization can be satisfied, including the
        // token admin's own transfer.
        env.mock_all_auths();

        let payer = Address::generate(&env);
        let token_client = token::Client::new(&env, &token_address);
        token_client.transfer(&token_admin, &payer, &TEST_BALANCE);

        (env, client, admin, token_address, payer)
    }

    /// Advance the test ledger, which is what drives stream accrual.
    fn advance(env: &Env, ledgers: u32) {
        env.ledger().with_mut(|li| li.sequence_number += ledgers);
    }

    /// Assert that a `try_*` call failed with a specific `ContractError`.
    ///
    /// Generated `try_*` methods return `Result<Result<T, _>, Result<Error, InvokeError>>`,
    /// so the contract-side error is the `Err` of the outer error. Comparing the
    /// discriminant (rather than a message string) keeps these assertions stable.
    #[track_caller]
    fn assert_contract_error<T, E>(
        result: Result<Result<T, E>, Result<soroban_sdk::Error, soroban_sdk::InvokeError>>,
        expected: ContractError,
    ) {
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected the call to fail with {:?}", expected),
        };
        // A contract-side `panic_with_error` surfaces as `Ok(Error(Contract, #n))`
        // inside the outer `Err` (see the `try_` docs in soroban-sdk's lib.rs).
        // Comparing the `#[contracterror]` discriminant rather than a message
        // keeps these assertions stable.
        match err {
            Ok(actual) => assert_eq!(
                actual,
                soroban_sdk::Error::from(expected),
                "wrong contract error: got {:?}, want {:?}",
                actual,
                expected
            ),
            other => panic!("unexpected failure: {:?} (wanted {:?})", other, expected),
        }
    }

    #[test]
    fn test_initialize() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&admin);

        assert_eq!(client.get_admin(), admin);
    }

    #[test]
    #[should_panic(expected = "Contract already initialized")]
    fn test_double_initialize_fails() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&admin);
        client.initialize(&admin); // should panic
    }

    #[test]
    fn test_mint_receipt() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&admin);

        let payer = Address::generate(&env);
        let payee = Address::generate(&env);

        env.mock_all_auths();

        let memo = Symbol::new(&env, "Rent");
        let receipt_id = client.mint_receipt(&payer, &payee, &1000, &memo);
        assert_eq!(receipt_id, 0);

        assert_eq!(client.get_receipt_count(&payer), 1);

        let stored = client.get_receipt(&payer, &0);
        assert_eq!(stored.from, payer);
        assert_eq!(stored.to, payee);
        assert_eq!(stored.amount, 1000);
        assert_eq!(stored.memo, memo);
    }

    #[test]
    fn test_receipt_count_tracks_multiple_mints() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&admin);

        let payer = Address::generate(&env);
        let payee1 = Address::generate(&env);
        let payee2 = Address::generate(&env);

        env.mock_all_auths();

        let id1 = client.mint_receipt(&payer, &payee1, &500, &Symbol::new(&env, "Coffee"));
        let id2 = client.mint_receipt(&payer, &payee2, &1500, &Symbol::new(&env, "Invoice"));

        assert_eq!(id1, 0);
        assert_eq!(id2, 1);
        assert_eq!(client.get_receipt_count(&payer), 2);
    }

    #[test]
    fn test_tip_totals_start_at_zero() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&admin);

        let recipient = Address::generate(&env);
        assert_eq!(client.get_tip_total(&recipient), 0);
        assert_eq!(client.get_tip_count(&recipient), 0);
    }

    // ── Ticket 1: max rate cap ──────────────────────────────────────────────

    #[test]
    fn test_max_rate_disabled_by_default() {
        let (env, client, _admin, token_addr, payer) = setup();

        // 0 means uncapped, so even a rate far above any sane bound is allowed.
        assert_eq!(client.get_max_rate(), 0);

        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &1_000_000, &1_000_000);
        assert_eq!(id, 0);
    }

    #[test]
    fn test_rate_below_cap_is_accepted() {
        let (env, client, admin, token_addr, payer) = setup();
        client.set_max_rate(&admin, &1_000);

        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &999, &100_000);
        assert_eq!(id, 0);
    }

    #[test]
    fn test_rate_exactly_at_cap_is_accepted() {
        let (env, client, admin, token_addr, payer) = setup();
        client.set_max_rate(&admin, &1_000);

        let recipient = Address::generate(&env);
        // At the cap is allowed: only strictly-greater rates are rejected.
        let id = client.open_stream(&token_addr, &payer, &recipient, &1_000, &100_000);
        assert_eq!(id, 0);
    }

    #[test]
    fn test_rate_above_cap_panics() {
        let (env, client, admin, token_addr, payer) = setup();
        client.set_max_rate(&admin, &1_000);

        let recipient = Address::generate(&env);

        assert_contract_error(client
            .try_open_stream(&token_addr, &payer, &recipient, &1_001, &100_000), ContractError::RateTooHigh);
    }

    #[test]
    fn test_set_max_rate_requires_admin() {
        let (env, client, _admin, _token_addr, _payer) = setup();
        let stranger = Address::generate(&env);

        assert_contract_error(client.try_set_max_rate(&stranger, &1_000), ContractError::NotAdmin);
    }

    #[test]
    fn test_set_max_rate_rejects_negative() {
        let (_env, client, admin, _token_addr, _payer) = setup();

        assert_contract_error(client.try_set_max_rate(&admin, &-1), ContractError::InvalidRate);
    }

    // ── Ticket 2: claim arithmetic ──────────────────────────────────────────

    #[test]
    fn test_open_stream() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);

        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);
        assert_eq!(id, 0);
        assert_eq!(client.get_stream_count(), 1);

        let s = client.get_stream(&id);
        assert_eq!(s.payer, payer);
        assert_eq!(s.recipient, recipient);
        assert_eq!(s.rate_per_ledger, 10);
        assert_eq!(s.deposited, 1_000);
        assert_eq!(s.claimed, 0);
    }

    #[test]
    fn test_claim_stream_basic() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);

        advance(&env, 5);
        assert_eq!(client.claim_stream(&id, &recipient), 50);
        assert_eq!(client.get_stream(&id).claimed, 50);
        assert_eq!(client.get_claimable(&id), 0);
    }

    #[test]
    fn test_claim_stream_multiple_times() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);

        advance(&env, 3);
        assert_eq!(client.claim_stream(&id, &recipient), 30);

        advance(&env, 4);
        assert_eq!(client.claim_stream(&id, &recipient), 40);

        assert_eq!(client.get_stream(&id).claimed, 70);
    }

    #[test]
    fn test_claim_stream_exceeds_deposit() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        // 100 ledgers of accrual against a 1_000 deposit would be 10_000.
        let id = client.open_stream(&token_addr, &payer, &recipient, &100, &1_000);

        advance(&env, 100);
        // Capped at the deposit, never 10_000.
        assert_eq!(client.claim_stream(&id, &recipient), 1_000);
        assert_eq!(client.get_stream(&id).claimed, 1_000);

        // Nothing left, and no second payout.
        assert_eq!(client.get_claimable(&id), 0);
        assert_contract_error(client.try_claim_stream(&id, &recipient), ContractError::StreamClosed);
    }

    #[test]
    fn test_top_up_stream() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);

        client.top_up_stream(&id, &payer, &500);
        assert_eq!(client.get_stream(&id).deposited, 1_500);
    }

    #[test]
    fn test_close_stream_with_refund() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);

        advance(&env, 10);
        client.claim_stream(&id, &recipient);

        // 1_000 deposited, 100 claimed, 100 accrued → 900 refunded.
        let refund = client.close_stream(&id, &payer);
        assert_eq!(refund, 900);
    }

    #[test]
    fn test_close_stream_after_claims() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);

        advance(&env, 50);
        let claimed = client.claim_stream(&id, &recipient);
        let refund = client.close_stream(&id, &payer);

        // The core conservation invariant: the two payouts split the deposit
        // exactly, with nothing stranded or minted.
        assert_eq!(claimed + refund, 1_000);
    }

    #[test]
    fn test_get_claimable() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);

        assert_eq!(client.get_claimable(&id), 0);
        advance(&env, 7);
        assert_eq!(client.get_claimable(&id), 70);
        // Reading claimable must not mutate state.
        assert_eq!(client.get_stream(&id).claimed, 0);
    }

    #[test]
    fn test_claim_nonexistent_stream() {
        let (env, client, _admin, _token_addr, _payer) = setup();
        let recipient = Address::generate(&env);

        assert_contract_error(
            client.try_claim_stream(&99, &recipient),
            ContractError::StreamNotFound,
        );
    }

    #[test]
    fn test_unauthorized_claim() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);

        advance(&env, 5);
        let stranger = Address::generate(&env);
        assert_contract_error(client.try_claim_stream(&id, &stranger), ContractError::NotRecipient);
    }

    #[test]
    fn test_unauthorized_close() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);

        let stranger = Address::generate(&env);
        assert_contract_error(client.try_close_stream(&id, &stranger), ContractError::NotPayer);
    }

    #[test]
    fn test_invalid_rate() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);

        assert_contract_error(client
            .try_open_stream(&token_addr, &payer, &recipient, &0, &1_000), ContractError::InvalidRate);

        assert_contract_error(client
            .try_open_stream(&token_addr, &payer, &recipient, &-5, &1_000), ContractError::InvalidRate);
    }

    #[test]
    fn test_invalid_deposit() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);

        assert_contract_error(client
            .try_open_stream(&token_addr, &payer, &recipient, &10, &0), ContractError::InvalidDeposit);
    }

    #[test]
    fn test_claim_nothing_accrued_panics() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);

        // No ledger elapsed, so there is nothing to withdraw.
        assert_contract_error(client.try_claim_stream(&id, &recipient), ContractError::NothingToClaim);
    }

    // ── Ticket 2: claim_stream arithmetic fuzz ─────────────────────────────

    // /// Property test over the accrual arithmetic.
    // ///
    // /// Exercises `total_streamed_amount` / `claimable_amount` directly with
    // /// random `(rate_per_ledger, elapsed, deposited, already_claimed)` inputs
    // /// drawn from the full `i128`/`u32` space — including the degenerate shapes
    // /// that hand-written cases miss: `rate > deposited` (funds zero ledgers),
    // /// `rate = 1` with a near-`i128::MAX` deposit, and elapsed ledgers far
    // /// beyond the funded window.
    // ///
    // /// Two properties are asserted for every input:
    // ///   1. no arithmetic panic (the whole point — `saturating_*` and the
    // ///      funded-ledger cap must hold for *any* value, not just realistic ones)
    // ///   2. `claimable <= deposited - already_claimed`, so repeated claims can
    // ///      never withdraw more than the escrow holds
    // ///
    // /// Run with `cargo test fuzz -- --nocapture`.
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        #[test]
        fn fuzz_claim_math_holds_for_arbitrary_inputs(
            rate_per_ledger in 1i128..=i128::MAX,
            deposited in 1i128..=i128::MAX,
            elapsed in 0u32..=u32::MAX,
            already_claimed in 0i128..=i128::MAX,
        ) {
            let env = Env::default();
            let stream = Stream {
                payer: Address::generate(&env),
                recipient: Address::generate(&env),
                rate_per_ledger,
                deposited,
                claimed: already_claimed,
                start_ledger: 0,
                token: Address::generate(&env),
            };

            // Current ledger is `elapsed`; start_ledger is 0. Property 1: these
            // calls must return rather than panic, for any input whatsoever.
            let total_streamed = total_streamed_amount(&stream, elapsed);
            let claimable = claimable_amount(&stream, elapsed);

            // Property 2: the structural bound. The escrow left is
            // `deposited - already_claimed`, floored at 0 — a stream that has
            // already paid out more than it holds has nothing left to offer.
            // (`saturating_sub` alone is not enough here: it saturates at
            // `i128::MIN`, not 0, so an over-claimed stream would produce a
            // large negative remainder.)
            let remaining = deposited.saturating_sub(already_claimed).max(0);
            assert!(
                claimable <= remaining,
                "claimable {} exceeded remaining {} (rate={}, deposited={}, elapsed={}, claimed={})",
                claimable,
                remaining,
                rate_per_ledger,
                deposited,
                elapsed,
                already_claimed
            );

            // Supporting invariants that make the bound meaningful.
            assert!(
                total_streamed <= deposited,
                "total_streamed {} exceeded deposited {}",
                total_streamed,
                deposited
            );
            assert!(claimable >= 0, "claimable must never be negative");
        }
    }

    // /// Property test over a full claim lifecycle driven through the contract.
    // ///
    // /// Opens a real stream, then claims repeatedly at randomized ledger
    // /// advances and top-ups. After every step the conservation invariant
    // /// `withdrawn <= deposited` is re-checked, and the final refund is checked
    // /// to make `withdrawn + refund == deposited` exactly, which catches both
    // /// over-payment and stranded funds.
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn fuzz_repeated_claims_never_exceed_deposit(
            rate_per_ledger in 1i128..=1_000i128,
            deposit in 1i128..=1_000_000i128,
            steps in proptest::collection::vec((1u32..50u32, 1i128..1_000i128), 1..12),
        ) {
            let (env, client, _admin, token_addr, payer) = setup();
            let recipient = Address::generate(&env);

            // The admin rate cap is off by default, so any positive rate opens.
            let id = client.open_stream(&token_addr, &payer, &recipient, &rate_per_ledger, &deposit);

            let mut withdrawn = 0i128;

            for (advance_by, top_up) in steps {
                advance(&env, advance_by);
                client.top_up_stream(&id, &payer, &top_up);

                // A claim with nothing accrued is a rejected no-op, not an
                // invariant violation, so skip rather than fail.
                if client.get_claimable(&id) == 0 {
                    continue;
                }

                let claimed = client.claim_stream(&id, &recipient);
                withdrawn = withdrawn.saturating_add(claimed);

                let stream = client.get_stream(&id);
                // Ticket invariant: a single claim never exceeds what the
                // stream still owes on top of what was already withdrawn.
                let outstanding = stream.deposited.saturating_sub(withdrawn - claimed);
                assert!(
                    claimed <= outstanding,
                    "claim {} exceeded outstanding {} (rate={}, deposit={})",
                    claimed,
                    outstanding,
                    rate_per_ledger,
                    deposit
                );
                assert!(
                    withdrawn <= stream.deposited,
                    "cumulative claims {} exceeded deposit {}",
                    withdrawn,
                    stream.deposited
                );
            }

            // Conservation: claims plus the final refund split the deposit exactly.
            let refund = client.close_stream(&id, &payer);
            assert_eq!(
                withdrawn + refund,
                client.get_stream(&id).deposited,
                "claims + refund must equal the deposit"
            );
        }
    }

    // ── Ticket 3: stream event log ─────────────────────────────────────────

    #[test]
    fn test_stream_history_starts_empty() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);

        assert_eq!(client.get_stream_history(&id).len(), 0);
    }

    #[test]
    fn test_stream_history_records_claims_and_topups() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);

        // 3 claims and 2 top-ups, interleaved.
        advance(&env, 1);
        client.claim_stream(&id, &recipient);
        client.top_up_stream(&id, &payer, &250);

        advance(&env, 2);
        client.claim_stream(&id, &recipient);
        client.top_up_stream(&id, &payer, &750);

        advance(&env, 3);
        client.claim_stream(&id, &recipient);

        let history = client.get_stream_history(&id);
        assert_eq!(history.len(), 5);

        // Chronological, with the right kind and amount on each entry.
        let expected: [(StreamEventType, i128); 5] = [
            (StreamEventType::Claim, 10),
            (StreamEventType::TopUp, 250),
            (StreamEventType::Claim, 20),
            (StreamEventType::TopUp, 750),
            (StreamEventType::Claim, 30),
        ];
        for (i, (kind, amount)) in expected.iter().enumerate() {
            let e = history.get(i as u32).unwrap();
            assert_eq!(&e.event_type, kind, "event_type at index {}", i);
            assert_eq!(e.amount, *amount, "amount at index {}", i);
        }

        // Ledger numbers are recorded and non-decreasing.
        let first = history.get(0).unwrap().ledger;
        let last = history.get(4).unwrap().ledger;
        assert!(last >= first);
    }

    #[test]
    fn test_stream_history_records_close() {
        let (env, client, _admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);

        advance(&env, 4);
        client.claim_stream(&id, &recipient);
        let refund = client.close_stream(&id, &payer);

        let history = client.get_stream_history(&id);
        assert_eq!(history.len(), 2);

        let close = history.get(1).unwrap();
        assert_eq!(close.event_type, StreamEventType::Close);
        assert_eq!(close.amount, refund);
    }

    #[test]
    fn test_stream_history_is_per_stream() {
        let (env, client, _admin, token_addr, payer) = setup();
        let r1 = Address::generate(&env);
        let r2 = Address::generate(&env);

        let a = client.open_stream(&token_addr, &payer, &r1, &10, &1_000);
        let b = client.open_stream(&token_addr, &payer, &r2, &10, &1_000);

        advance(&env, 1);
        client.claim_stream(&a, &r1);

        assert_eq!(client.get_stream_history(&a).len(), 1);
        assert_eq!(client.get_stream_history(&b).len(), 0);
    }

    // ── Ticket 4: freeze / unfreeze ────────────────────────────────────────

    #[test]
    fn test_freeze_blocks_state_changes() {
        let (env, client, admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);

        advance(&env, 5);
        client.freeze(&admin);
        assert!(client.is_frozen());

        // Every state-changing entry point rejects the call.
        let frozen = ContractError::Frozen;
        assert_contract_error(client.try_claim_stream(&id, &recipient), frozen);
        assert_contract_error(
            client.try_open_stream(&token_addr, &payer, &recipient, &10, &1_000),
            frozen,
        );
        assert_contract_error(client.try_top_up_stream(&id, &payer, &100), frozen);
        assert_contract_error(client.try_close_stream(&id, &payer), frozen);
        assert_contract_error(client.try_set_max_rate(&admin, &500), frozen);
        assert_contract_error(
            client.try_mint_receipt(&payer, &payer, &1, &Symbol::new(&env, "n")),
            frozen,
        );
        assert_contract_error(client.try_send_tip(&token_addr, &payer, &recipient, &1), frozen);
    }

    #[test]
    fn test_read_only_works_while_frozen() {
        let (env, client, admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);
        advance(&env, 5);

        client.freeze(&admin);

        // Monitoring still works during an incident.
        assert_eq!(client.get_stream(&id).deposited, 1_000);
        assert_eq!(client.get_claimable(&id), 50);
        assert_eq!(client.get_stream_count(), 1);
        assert_eq!(client.get_admin(), admin);
        assert_eq!(client.get_max_rate(), 0);
        assert!(client.is_frozen());
    }

    #[test]
    fn test_unfreeze_restores_state_changes() {
        let (env, client, admin, token_addr, payer) = setup();
        let recipient = Address::generate(&env);
        let id = client.open_stream(&token_addr, &payer, &recipient, &10, &1_000);

        client.freeze(&admin);
        client.unfreeze(&admin);
        assert!(!client.is_frozen());

        // Accrual that happened during the freeze is claimable again.
        advance(&env, 5);
        assert_eq!(client.claim_stream(&id, &recipient), 50);
    }

    #[test]
    fn test_freeze_requires_admin() {
        let (env, client, _admin, _token_addr, _payer) = setup();
        let stranger = Address::generate(&env);

        assert_contract_error(client.try_freeze(&stranger), ContractError::NotAdmin);
        assert!(!client.is_frozen());

        assert_contract_error(client.try_unfreeze(&stranger), ContractError::NotAdmin);
    }

    #[test]
    fn test_freeze_twice_is_idempotent() {
        let (_env, client, admin, _token_addr, _payer) = setup();
        client.freeze(&admin);
        client.freeze(&admin);
        assert!(client.is_frozen());
    }
}
