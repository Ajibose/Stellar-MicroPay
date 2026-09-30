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
    contract, contractimpl, contracttype,
    token, Address, BytesN, Env, Symbol,
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
    /// Operator fee, in basis points, charged on every tip
    FeeBps,
}

/// Event payload emitted when a tip is sent, capturing the gross tip
/// amount and any operator fee that was deducted from it.
#[contracttype]
#[derive(Clone, Debug)]
pub struct TipEventData {
    pub amount: i128,
    pub fee_amount: i128,
}

/// Maximum operator fee the admin may configure, in basis points (5%).
const MAX_FEE_BPS: u32 = 500;

/// Basis-point denominator: 1 bps = 1 / 10_000.
const FEE_BPS_DENOMINATOR: i128 = 10_000;

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

    // ─── Admin & Upgrade ─────────────────────────────────────────────────────

    /// Upgrade the WASM code of the current contract.
    /// Admin-gated: only stored Admin address can call this function.
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("Contract not initialized");
        admin.require_auth();

        env.deployer().update_current_contract_wasm(new_wasm_hash);
    }

    /// Rotate/update the contract admin address.
    /// Admin-gated: only current Admin address can set a new admin.
    pub fn set_admin(env: Env, new_admin: Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("Contract not initialized");
        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &new_admin);
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
    /// If an operator fee is configured (see `set_fee_bps`), `amount * fee_bps / 10000`
    /// is transferred to the admin and the remainder to `to`. This records the tip
    /// on-chain for analytics and emits an event.
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

        let fee_bps: u32 = env
            .storage()
            .instance()
            .get(&DataKey::FeeBps)
            .unwrap_or(0);
        let fee_amount: i128 = (amount * fee_bps as i128) / FEE_BPS_DENOMINATOR;
        let net_amount: i128 = amount - fee_amount;

        // Transfer tokens via the Stellar token interface (SAC)
        let token = token::Client::new(&env, &token_address);

        if fee_amount > 0 {
            let admin: Address = env
                .storage()
                .instance()
                .get(&DataKey::Admin)
                .expect("Contract not initialized");
            token.transfer(&from, &admin, &fee_amount);
        }

        token.transfer(&from, &to, &net_amount);

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

        // Emit an event for indexers, including the fee collected (if any)
        env.events().publish(
            (Symbol::new(&env, "tip"), from, to.clone()),
            TipEventData { amount, fee_amount },
        );
    }

    // ─── Fees ────────────────────────────────────────────────────────────────

    /// Set the operator fee charged on every tip, in basis points
    /// (1 bps = 0.01%). Capped at 500 bps (5%). Only callable by the
    /// current admin. A `fee_bps` of 0 disables the fee (the default).
    pub fn set_fee_bps(env: Env, admin: Address, fee_bps: u32) {
        admin.require_auth();

        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("Contract not initialized");

        if admin != stored_admin {
            panic!("Only the admin can set the fee");
        }

        if fee_bps > MAX_FEE_BPS {
            panic!("Fee exceeds maximum allowed (500 bps)");
        }

        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
    }

    /// Get the currently configured operator fee, in basis points.
    pub fn get_fee_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::FeeBps)
            .unwrap_or(0)
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
    fn test_mint_receipt_stores_receipt_record_accessible_via_get_receipt() {
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
        assert_eq!(stored.ledger, env.ledger().sequence());
    }

    #[test]
    fn test_mint_two_receipts_from_same_payer_increments_count() {
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

        let receipt1 = client.get_receipt(&payer, &0);
        assert_eq!(receipt1.to, payee1);
        assert_eq!(receipt1.amount, 500);

        let receipt2 = client.get_receipt(&payer, &1);
        assert_eq!(receipt2.to, payee2);
        assert_eq!(receipt2.amount, 1500);
    }

    #[test]
    #[should_panic(expected = "Receipt not found")]
    fn test_get_receipt_out_of_range_panics_gracefully() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&admin);

        let payer = Address::generate(&env);
        client.get_receipt(&payer, &0); // No receipts minted yet -> panics
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

    #[test]
    fn test_set_admin_and_rotation() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&admin);
        assert_eq!(client.get_admin(), admin);

        let new_admin = Address::generate(&env);

        env.mock_all_auths();
        client.set_admin(&new_admin);

        assert_eq!(client.get_admin(), new_admin);
    }

    #[test]
    #[should_panic]
    fn test_upgrade_non_admin_panics() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&admin);

        let dummy_hash = BytesN::from_array(&env, &[1u8; 32]);
        // Without admin auth, calling upgrade panics
        client.upgrade(&dummy_hash);
    }

    #[test]
    fn test_upgrade_admin_requires_auth_and_invokes_deployer() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&admin);

        env.mock_all_auths();

        let dummy_hash = BytesN::from_array(&env, &[1u8; 32]);
        // With admin auth, try_upgrade passes the admin auth check and invokes deployer.
        // In native test environment, deployer returns an Err (InvalidAction for non-wasm target).
        let res = client.try_upgrade(&dummy_hash);
        assert!(res.is_err());
    }





}

