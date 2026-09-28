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
    token, Address, Env, Symbol,
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

/// A streaming payment channel: `payer` deposits `deposited` tokens and the
/// stream accrues `rate_per_ledger` per ledger for the `recipient`.
#[contracttype]
#[derive(Clone, Debug)]
pub struct Stream {
    /// The payer who locked the deposit (can close / top-up the stream).
    pub payer: Address,
    /// The single recipient of the streamed funds.
    pub recipient: Address,
    /// Amount streamed per ledger (in stroops).
    pub rate_per_ledger: i128,
    /// Total amount deposited (in stroops).
    pub deposited: i128,
    /// Total amount already claimed by the recipient (in stroops).
    pub claimed: i128,
    /// Ledger number when the stream was opened.
    pub start_ledger: u32,
    /// Token contract the deposit is denominated in.
    pub token: Address,
    /// True once `close_stream` has settled and refunded the stream.
    pub closed: bool,
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
    /// Running counter of created streams (next stream id).
    StreamCount,
    /// A single stream, indexed by id.
    Stream(u32),
}

// ─── Streaming helpers ─────────────────────────────────────────────────────────

/// Total amount streamed to the recipient so far, as of `current_ledger`.
///
/// `rate_per_ledger * elapsed` is capped at `deposited` so the result can never
/// exceed what was deposited — and the `checked_mul` prevents i128 overflow when
/// the rate or elapsed value is very large.
fn total_streamed_amount(stream: &Stream, current_ledger: u32) -> i128 {
    let elapsed = u64::from(current_ledger.saturating_sub(stream.start_ledger));
    let total = stream.rate_per_ledger.checked_mul(elapsed as i128);
    match total {
        Some(t) => t.min(stream.deposited),
        None => stream.deposited,
    }
}

/// Amount the recipient can withdraw right now: total streamed minus already
/// claimed, clamped to zero.
fn claimable_amount(stream: &Stream, current_ledger: u32) -> i128 {
    if stream.closed {
        return 0;
    }
    let claimable = total_streamed_amount(stream, current_ledger) - stream.claimed;
    if claimable > 0 {
        claimable
    } else {
        0
    }
}

fn load_stream(env: &Env, stream_id: u32) -> Stream {
    env.storage()
        .persistent()
        .get(&DataKey::Stream(stream_id))
        .expect("stream not found")
}

fn save_stream(env: &Env, stream_id: u32, stream: &Stream) {
    env.storage().persistent().set(&DataKey::Stream(stream_id), stream);
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
        _env: Env,
        _from: Address,
        _to: Address,
        _amount: i128,
        _release_ledger: u32,
    ) {
        panic!("Escrow payments coming in v2.1 — see ROADMAP.md");
    }

    /// [PLACEHOLDER] Batch multiple micro-payments in a single transaction.
    /// See ROADMAP.md v2.0 — Multi-Currency Payments.
    pub fn batch_send(
        _env: Env,
        _from: Address,
        _recipients: soroban_sdk::Vec<Address>,
        _amounts: soroban_sdk::Vec<i128>,
    ) {
        panic!("Batch payments coming in v2.0 — see ROADMAP.md");
    }

    // ─── Streaming payments ───────────────────────────────────────────────────

    /// Open a payment stream, locking `deposit` in the contract. The recipient
    /// can claim `rate_per_ledger` per ledger; on `close_stream` the unstreamed
    /// portion is refunded to the payer.
    ///
    /// Returns the new stream id.
    pub fn open_stream(
        env: Env,
        token_address: Address,
        payer: Address,
        recipient: Address,
        rate_per_ledger: i128,
        deposit: i128,
    ) -> u32 {
        payer.require_auth();
        if rate_per_ledger <= 0 {
            panic!("rate_per_ledger must be positive");
        }
        if deposit <= 0 {
            panic!("deposit must be positive");
        }

        let token = token::Client::new(&env, &token_address);
        token.transfer(&payer, &env.current_contract_address(), &deposit);

        let stream_id: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::StreamCount)
            .unwrap_or(0);
        let stream = Stream {
            payer: payer.clone(),
            recipient: recipient.clone(),
            rate_per_ledger,
            deposited: deposit,
            claimed: 0,
            start_ledger: env.ledger().sequence(),
            token: token_address,
            closed: false,
        };
        save_stream(&env, stream_id, &stream);
        env.storage()
            .persistent()
            .set(&DataKey::StreamCount, &(stream_id + 1));

        env.events().publish(
            (Symbol::new(&env, "stream_open"), stream_id),
            (payer, recipient, rate_per_ledger, deposit),
        );
        stream_id
    }

    /// Withdraw everything accrued so far for the calling recipient.
    /// Returns the amount transferred, which is `0` when nothing has accrued
    /// since the last claim or the stream is closed.
    pub fn claim_stream(env: Env, stream_id: u32, recipient: Address) -> i128 {
        recipient.require_auth();
        let mut stream = load_stream(&env, stream_id);
        if stream.recipient != recipient {
            panic!("unauthorized");
        }
        if stream.closed {
            panic!("stream is closed");
        }

        let amount = claimable_amount(&stream, env.ledger().sequence());
        if amount == 0 {
            return 0;
        }
        stream.claimed += amount;
        save_stream(&env, stream_id, &stream);

        let token = token::Client::new(&env, &stream.token);
        token.transfer(&env.current_contract_address(), &recipient, &amount);

        env.events().publish(
            (Symbol::new(&env, "stream_claim"), stream_id),
            (recipient, amount),
        );
        amount
    }

    /// Add funds to an open stream, extending how long it can run.
    pub fn top_up_stream(env: Env, stream_id: u32, payer: Address, amount: i128) {
        payer.require_auth();
        let mut stream = load_stream(&env, stream_id);
        if stream.payer != payer {
            panic!("unauthorized");
        }
        if stream.closed {
            panic!("stream is closed");
        }
        if amount <= 0 {
            panic!("amount must be positive");
        }

        let token = token::Client::new(&env, &stream.token);
        token.transfer(&payer, &env.current_contract_address(), &amount);

        stream.deposited += amount;
        save_stream(&env, stream_id, &stream);

        env.events().publish(
            (Symbol::new(&env, "stream_topup"), stream_id),
            (payer, amount, stream.deposited),
        );
    }

    /// Stop a stream: transfer all accrued funds to the recipient and refund
    /// the unstreamed remainder to the payer.
    ///
    /// **Mathematical invariant:** after `close_stream` returns,
    /// `refund + claimed == deposited` holds by construction — the recipient
    /// receives every streamable stroop (`claimed` rises to `total_streamed`),
    /// and the payer receives the balance (`refund = deposited - claimed`).
    ///
    /// Returns the refund amount.
    pub fn close_stream(env: Env, stream_id: u32, payer: Address) -> i128 {
        payer.require_auth();
        let mut stream = load_stream(&env, stream_id);
        if stream.payer != payer {
            panic!("unauthorized");
        }
        if stream.closed {
            panic!("stream is closed");
        }

        let current_ledger = env.ledger().sequence();
        let total_streamed = total_streamed_amount(&stream, current_ledger);

        let token = token::Client::new(&env, &stream.token);
        let contract_address = env.current_contract_address();

        // Transfer the streamed-but-not-yet-claimed portion to the recipient.
        let pending = total_streamed - stream.claimed;
        if pending > 0 {
            token.transfer(&contract_address, &stream.recipient, &pending);
        }

        // After close the recipient has effectively "claimed" everything that
        // was ever streamable.
        stream.claimed = total_streamed;

        // Refund = what was deposited minus everything the recipient now holds.
        let refund = stream.deposited - stream.claimed;
        if refund > 0 {
            token.transfer(&contract_address, &payer, &refund);
        }

        stream.closed = true;
        save_stream(&env, stream_id, &stream);

        env.events().publish(
            (Symbol::new(&env, "stream_close"), stream_id),
            (stream.claimed, refund),
        );

        refund
    }

    /// Get the stream record for a given id.
    pub fn get_stream(env: Env, stream_id: u32) -> Stream {
        load_stream(&env, stream_id)
    }

    /// Amount the recipient could withdraw right now (without claiming).
    pub fn get_claimable(env: Env, stream_id: u32, recipient: Address) -> i128 {
        let stream = load_stream(&env, stream_id);
        if stream.recipient != recipient {
            panic!("unauthorized");
        }
        claimable_amount(&stream, env.ledger().sequence())
    }

    /// Total number of streams created so far.
    pub fn get_stream_count(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::StreamCount)
            .unwrap_or(0)
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Ledger as _},
        token, Address, Env,
    };

    fn advance_by(env: &Env, ledgers: u32) {
        env.ledger().with_mut(|info| {
            info.sequence_number = info.sequence_number.saturating_add(ledgers);
        });
    }

    fn stream_fixture(
        env: &Env,
        funding: i128,
    ) -> (Address, MicroPayContractClient<'_>, Address, Address, Address) {
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(env, &contract_id);
        let admin = Address::generate(env);
        client.initialize(&admin);

        let payer = Address::generate(env);
        let recipient = Address::generate(env);
        env.mock_all_auths();
        let token_id = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        token::StellarAssetClient::new(env, &token_id).mint(&payer, &funding);

        (contract_id, client, token_id, payer, recipient)
    }

    fn claimed_of(stream: &Stream) -> i128 {
        stream.claimed
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

    #[test]
    fn test_open_stream() {
        let env = Env::default();
        let (contract_id, client, token_id, payer, recipient) = stream_fixture(&env, 100_000);
        let token = token::Client::new(&env, &token_id);

        let rate: i128 = 100;
        let deposit: i128 = 100_000;
        let id = client.open_stream(&token_id, &payer, &recipient, &rate, &deposit);

        assert_eq!(id, 0);
        assert_eq!(client.get_stream_count(), 1);

        assert_eq!(token.balance(&payer), 0);
        assert_eq!(token.balance(&contract_id), deposit);
        let stream = client.get_stream(&id);
        assert_eq!(stream.payer, payer);
        assert_eq!(stream.recipient, recipient);
        assert_eq!(stream.rate_per_ledger, rate);
        assert_eq!(stream.deposited, deposit);
        assert_eq!(stream.claimed, 0);
        assert_eq!(stream.start_ledger, env.ledger().sequence());
        assert!(!stream.closed);
    }

    #[test]
    fn test_claim_stream() {
        let env = Env::default();
        let (contract_id, client, token_id, payer, recipient) = stream_fixture(&env, 100_000);
        let token = token::Client::new(&env, &token_id);

        let rate: i128 = 100;
        let deposit: i128 = 100_000;
        let id = client.open_stream(&token_id, &payer, &recipient, &rate, &deposit);

        advance_by(&env, 10);

        let claimed = client.claim_stream(&id, &recipient);
        assert_eq!(claimed, rate * 10);
        assert_eq!(token.balance(&recipient), rate * 10);
        assert_eq!(token.balance(&contract_id), deposit - rate * 10);
        assert_eq!(claimed_of(&client.get_stream(&id)), rate * 10);
    }

    #[test]
    fn test_claim_stream_capped_at_deposit() {
        let env = Env::default();
        let (contract_id, client, token_id, payer, recipient) = stream_fixture(&env, 100_000);
        let token = token::Client::new(&env, &token_id);

        let rate: i128 = 100;
        let deposit: i128 = 100_000;
        let id = client.open_stream(&token_id, &payer, &recipient, &rate, &deposit);

        advance_by(&env, 10_000);

        assert_eq!(client.get_claimable(&id, &recipient), deposit);
        assert_eq!(client.claim_stream(&id, &recipient), deposit);
        assert_eq!(client.claim_stream(&id, &recipient), 0);
        assert_eq!(token.balance(&recipient), deposit);
        assert_eq!(token.balance(&contract_id), 0);
    }

    #[test]
    fn test_close_stream_with_refund() {
        let env = Env::default();
        let (contract_id, client, token_id, payer, recipient) = stream_fixture(&env, 100_000);
        let token = token::Client::new(&env, &token_id);

        let rate: i128 = 100;
        let deposit: i128 = 100_000;
        let id = client.open_stream(&token_id, &payer, &recipient, &rate, &deposit);
        advance_by(&env, 20);

        let refund = client.close_stream(&id, &payer);
        let streamed = rate * 20;
        assert_eq!(refund, deposit - streamed);
        assert_eq!(token.balance(&recipient), streamed);
        assert_eq!(token.balance(&payer), refund);
        assert_eq!(token.balance(&contract_id), 0);
        assert!(client.get_stream(&id).closed);
    }

    #[test]
    fn test_close_stream_after_claims() {
        let env = Env::default();
        let (contract_id, client, token_id, payer, recipient) = stream_fixture(&env, 100_000);
        let token = token::Client::new(&env, &token_id);

        let rate: i128 = 100;
        let deposit: i128 = 100_000;
        let id = client.open_stream(&token_id, &payer, &recipient, &rate, &deposit);
        advance_by(&env, 30);
        client.claim_stream(&id, &recipient);
        advance_by(&env, 20);

        let refund = client.close_stream(&id, &payer);
        let streamed = rate * 50;
        assert_eq!(token.balance(&recipient), streamed);
        assert_eq!(token.balance(&payer), deposit - streamed);
        assert_eq!(token.balance(&contract_id), 0);
        assert_eq!(refund, deposit - streamed);
        assert_eq!(client.get_stream(&id).claimed, streamed);
    }

    // ─── Property test: close_stream invariant (#1085) ───────────────────────

    /// Deterministic linear congruential generator — no external crate needed.
    struct Lcg(u64);

    impl Lcg {
        fn new(seed: u64) -> Self {
            Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        }

        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        fn gen_range(&mut self, min: i128, max: i128) -> i128 {
            let span = max - min;
            min + (self.next_u64() as i128 % span)
        }

        fn gen_u32_range(&mut self, min: u32, max: u32) -> u32 {
            let span = (max - min) as u64;
            min + (self.next_u64() % span) as u32
        }
    }

    /// Helper that opens a stream, advances the ledger, closes it, then asserts
    /// the core mathematical invariant: `refund + claimed == deposited`.
    fn check_close_stream_invariant(rate: i128, elapsed: u32, deposit: i128) {
        let env = Env::default();
        let (_, client, token_id, payer, recipient) = stream_fixture(&env, deposit);

        let stream_id = client.open_stream(&token_id, &payer, &recipient, &rate, &deposit);

        if elapsed > 0 {
            advance_by(&env, elapsed);
        }

        let refund = client.close_stream(&stream_id, &payer);
        let stream = client.get_stream(&stream_id);

        assert_eq!(
            refund + stream.claimed,
            stream.deposited,
            "Invariant violated: rate={}, elapsed={}, deposit={}, refund={}, claimed={}",
            rate,
            elapsed,
            deposit,
            refund,
            stream.claimed
        );
    }

    /// Property test verifying the streaming contract invariant:
    /// `refund + claimed == deposited` — the refund returned by `close_stream`
    /// plus the recipient's `claimed` total must always equal the original
    /// `deposited` amount, regardless of rate, elapsed ledgers, or deposit size.
    #[test]
    fn test_close_stream_refund_plus_claimed_equals_deposited() {
        // ── Edge cases explicitly requested in the issue ──────────────────

        // 0 elapsed: nothing streamed, full refund.
        check_close_stream_invariant(1_000, 0, 1_000_000);

        // rate > deposit / ledger: deposit fully streamed on ledger 1,
        // refund is zero.
        check_close_stream_invariant(2_000_000, 1, 1_000_000);

        // Very large rate: rate * elapsed overflows i128, but
        // total_streamed_amount caps at `deposited` via checked_mul.
        check_close_stream_invariant(i128::MAX, 2, 1_000_000);

        // ── 100+ randomly generated (rate, elapsed, deposit) triples ────────
        let mut rng = Lcg::new(0xDEADBEEF_CAFEBABE);
        for _ in 0..100 {
            let rate = rng.gen_range(1, 1_000_001);
            let elapsed = rng.gen_u32_range(0, 50_000);
            let deposit = rng.gen_range(1, 1_000_001);
            check_close_stream_invariant(rate, elapsed, deposit);
        }
    }

    /// Property test with prior claims interleaved before close — the
    /// invariant must still hold even if the recipient has partially claimed.
    #[test]
    fn test_close_stream_refund_invariant_after_partial_claims() {
        let mut rng = Lcg::new(0x12345678_9ABCDEF0);
        for _ in 0..50 {
            let rate = rng.gen_range(1, 100_001);
            let deposit = rng.gen_range(10_000, 10_000_001);
            let env = Env::default();
            let (_, client, token_id, payer, recipient) = stream_fixture(&env, deposit);

            let stream_id = client.open_stream(&token_id, &payer, &recipient, &rate, &deposit);

            // Claim at some intermediate point.
            let first_elapsed = rng.gen_u32_range(1, 100);
            advance_by(&env, first_elapsed);
            let first_claim = client.claim_stream(&stream_id, &recipient);

            // Advance further, then close.
            let remaining = rng.gen_u32_range(0, 500);
            advance_by(&env, remaining);

            let refund = client.close_stream(&stream_id, &payer);
            let stream = client.get_stream(&stream_id);

            assert_eq!(
                refund + stream.claimed,
                stream.deposited,
                "Invariant violated: rate={}, first={}, remaining={}, deposit={}, \
                 first_claim={}, refund={}, claimed={}",
                rate,
                first_elapsed,
                remaining,
                deposit,
                first_claim,
                refund,
                stream.claimed
            );
        }
    }
}
