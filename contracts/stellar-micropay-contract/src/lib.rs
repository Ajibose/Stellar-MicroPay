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
 *   stellar contract build
 *   (or: cargo build --target wasm32v1-none --release from the workspace root;
 *    the wasm32-unknown-unknown target is unsupported by soroban-sdk 28)
 *
 * Deploy (Stellar CLI):
 *   stellar contract deploy \
 *     --wasm target/wasm32v1-none/release/stellar_micropay_contract.wasm \
 *     --source YOUR_SECRET_KEY \
 *     --network testnet
 */
use soroban_sdk::{contract, contractimpl, contracttype, token, Address, Env, Symbol};

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
}

// ─── Contract ─────────────────────────────────────────────────────────────────

#[contract]
pub struct MicroPayContract;

#[contractimpl]
impl MicroPayContract {
    // ─── Initialization ──────────────────────────────────────────────────────

    /// Initialize the contract with an admin address.
    /// Can only be called once.
    ///
    /// Security review (#1121):
    ///   - `admin` must authorize, so the stored admin address can never be
    ///     set to a value that the admin itself did not sign off on.
    ///   - Once written, the admin can never be changed: the duplicate-init
    ///     check rejects any later call before it reaches the write.
    ///
    /// Residual risk (documented, not changed by this review): `initialize`
    /// is a separate invocation from the deploy, so the first caller can
    /// still install *themselves* as admin — `require_auth` proves the stored
    /// admin consented to being admin, it cannot prove the address is the
    /// deployer. Deploy via a `__constructor` (deploy-time init) or invoke
    /// `initialize` in the same transaction as the deploy to close that
    /// window entirely.
    pub fn initialize(env: Env, admin: Address) {
        // Ensure not already initialized
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("Contract already initialized");
        }
        admin.require_auth();
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
    /// Security review (#1121):
    ///   - `from` must authorize, and that authorization covers all four
    ///     invocation arguments (token_address, from, to, amount), so an
    ///     existing auth entry cannot be replayed with a swapped sender,
    ///     recipient or amount.
    ///   - `amount` is validated *before* `require_auth` so a rejected call
    ///     leaves no satisfied authorization behind for `from`.
    ///   - The nested SAC `transfer` is authorized by the same `from`
    ///     authorization; `to` is never required to authorize.
    ///
    /// This records the tip on-chain for analytics and emits an event.
    pub fn send_tip(env: Env, token_address: Address, from: Address, to: Address, amount: i128) {
        // Validate amount before requesting authorization so that a failed
        // call can never double as a cross-contract authorization for `from`
        // (a failing require_auth does not roll back sibling effects in the
        // same transaction when this contract is called by another contract).
        if amount <= 0 {
            panic!("Tip amount must be positive");
        }

        // Require sender authorization
        from.require_auth();

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
        env.events()
            .publish((Symbol::new(&env, "tip"), from, to.clone()), amount);
    }

    // ─── Getters ─────────────────────────────────────────────────────────────

    /// Get the total amount tipped to a recipient (in stroops).
    ///
    /// Security review (#1121): read-only accessor — no `require_auth` is
    /// required because no state is written and the value is public.
    pub fn get_tip_total(env: Env, recipient: Address) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::TipTotal(recipient))
            .unwrap_or(0)
    }

    /// Get the number of tips received by a recipient.
    ///
    /// Security review (#1121): read-only accessor — no `require_auth` is
    /// required because no state is written and the value is public.
    pub fn get_tip_count(env: Env, recipient: Address) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::TipCount(recipient))
            .unwrap_or(0)
    }

    /// Get the contract admin address.
    ///
    /// Security review (#1121): read-only accessor — `get_admin` writes no
    /// state, so it needs no authorization. It reveals the admin address,
    /// which is already public on-chain.
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("Contract not initialized")
    }

    /// Get a specific tip record for a recipient by index.
    ///
    /// Security review (#1121): read-only accessor — no `require_auth` is
    /// required because no state is written. Tip records contain only public
    /// payment data.
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
    ///
    /// Security review (#1121):
    ///   - `from` must authorize, and that authorization covers all four
    ///     invocation arguments (from, to, amount, memo), so receipts cannot
    ///     be minted on behalf of an address that never signed.
    ///   - Receipts are keyed by the authenticated `from` (`ReceiptCount` /
    ///     `ReceiptRecord`), so a caller cannot append to another payer's
    ///     receipt sequence.
    ///   - `amount` is validated *before* `require_auth` (see `send_tip`).
    pub fn mint_receipt(env: Env, from: Address, to: Address, amount: i128, memo: Symbol) -> u32 {
        // Validate amount before requesting authorization (see `send_tip`).
        if amount <= 0 {
            panic!("Receipt amount must be positive");
        }

        from.require_auth();

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

        env.events()
            .publish((Symbol::new(&env, "receipt"), from), count);

        count
    }

    /// Get the total number of receipts minted for a payer.
    ///
    /// Security review (#1121): read-only accessor — no `require_auth` is
    /// required because no state is written and the count is public.
    pub fn get_receipt_count(env: Env, payer: Address) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::ReceiptCount(payer))
            .unwrap_or(0)
    }

    /// Get a specific receipt for a payer by index.
    ///
    /// Security review (#1121): read-only accessor — no `require_auth` is
    /// required because no state is written. Receipt metadata contains only
    /// public payment data.
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
    ///
    /// Security review (#1121): the stub panics before touching any state, so
    /// it requires no authorization today. The real implementation must
    /// `require_auth` from `from` (deposit and cancel) and from `to` on
    /// release, and must validate `amount` before requesting authorization.
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
    ///
    /// Security review (#1121): the stub panics before touching any state, so
    /// it requires no authorization today. The real implementation must
    /// `require_auth` from `from` (the single payer for the whole batch), and
    /// must validate every amount before requesting authorization.
    pub fn batch_send(
        _env: Env,
        _from: Address,
        _recipients: soroban_sdk::Vec<Address>,
        _amounts: soroban_sdk::Vec<i128>,
    ) {
        panic!("Batch payments coming in v2.0 — see ROADMAP.md");
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::{
            Address as _, AuthorizedFunction, AuthorizedInvocation, MockAuth, MockAuthInvoke,
        },
        Address, Env, IntoVal, TryFromVal,
    };

    #[test]
    fn test_initialize() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        env.mock_all_auths();

        client.initialize(&admin);

        assert_eq!(client.get_admin(), admin);
    }

    #[test]
    fn test_initialize_requires_admin_auth() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        env.mock_all_auths();

        client.initialize(&admin);

        // The admin must be the (only) authorizer of `initialize`.
        assert_eq!(
            env.auths(),
            [(
                admin.clone(),
                AuthorizedInvocation {
                    function: AuthorizedFunction::Contract((
                        client.address.clone(),
                        Symbol::new(&env, "initialize"),
                        (admin.clone(),).into_val(&env)
                    )),
                    sub_invocations: [].into(),
                }
            )]
        );
    }

    #[test]
    #[should_panic(expected = "Contract already initialized")]
    fn test_double_initialize_fails() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        env.mock_all_auths();
        client.initialize(&admin);
        client.initialize(&admin); // should panic
    }

    /// Issue #1121 acceptance criteria: an entry point must fail when the
    /// caller provides no authorization at all.
    #[test]
    fn test_initialize_rejects_unauthorized_admin() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);

        // No authorization is mocked, so `admin.require_auth()` must fail.
        let result = client.try_initialize(&admin);
        assert!(result.is_err());

        // Nothing was written: the contract is still uninitialized.
        assert!(client.try_get_admin().is_err());
    }

    /// Issue #1121 acceptance criteria: no operation may let an unauthorized
    /// address act as another — authorizing `attacker` must not install
    /// `victim` as admin.
    #[test]
    fn test_initialize_rejects_authorization_of_another_address() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let victim = Address::generate(&env);
        let attacker = Address::generate(&env);

        // Only `attacker` signs, while the admin being stored is `victim`.
        env.mock_auths(&[MockAuth {
            address: &attacker,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "initialize",
                args: (&victim,).into_val(&env),
                sub_invokes: &[],
            },
        }]);

        let result = client.try_initialize(&victim);
        assert!(result.is_err());
        assert!(client.try_get_admin().is_err());
    }

    #[test]
    fn test_mint_receipt() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        env.mock_all_auths();
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
        env.mock_all_auths();
        client.initialize(&admin);

        let payer = Address::generate(&env);
        let payee1 = Address::generate(&env);
        let payee2 = Address::generate(&env);

        let id1 = client.mint_receipt(&payer, &payee1, &500, &Symbol::new(&env, "Coffee"));
        let id2 = client.mint_receipt(&payer, &payee2, &1500, &Symbol::new(&env, "Invoice"));

        assert_eq!(id1, 0);
        assert_eq!(id2, 1);
        assert_eq!(client.get_receipt_count(&payer), 2);
    }

    /// End-to-end tip flow against a real (test) Stellar Asset Contract:
    /// verifies the transfer executes and that the sender is the authorizer
    /// of the `send_tip` invocation.
    #[test]
    fn test_send_tip_moves_funds_and_requires_from_auth() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        env.mock_all_auths();
        client.initialize(&admin);

        // Register a native-like SAC and fund the tip sender.
        let sac = env.register_stellar_asset_contract_v2(admin.clone());
        let sac_address = sac.address();
        let token = token::Client::new(&env, &sac_address);
        let sac_admin = token::StellarAssetClient::new(&env, &sac_address);

        let from = Address::generate(&env);
        let to = Address::generate(&env);
        let amount = 123_i128;
        sac_admin.mint(&from, &10_000);

        client.send_tip(&sac_address, &from, &to, &amount);

        // Exactly one auth entry is recorded and it belongs to the sender
        // at the `send_tip` root (the SAC transfer appears nested under it).
        // NOTE: `env.auths()` only reflects the most recent top-level
        // invocation, so this must run before any further client calls.
        let auths = env.auths();
        assert_eq!(auths.len(), 1);
        let (authorizer, invocation) = &auths[0];
        assert_eq!(authorizer, &from);
        match &invocation.function {
            AuthorizedFunction::Contract((contract_address, name, args)) => {
                assert_eq!(contract_address, &client.address);
                assert_eq!(name, &Symbol::new(&env, "send_tip"));
                assert_eq!(args.len(), 4);
                // (token, from, to, amount) — sender and amount must be
                // covered by the authorization to prevent argument swapping.
                assert_eq!(
                    Address::try_from_val(&env, &args.get(1).unwrap()).unwrap(),
                    from
                );
                assert_eq!(
                    Address::try_from_val(&env, &args.get(2).unwrap()).unwrap(),
                    to
                );
                assert_eq!(
                    i128::try_from_val(&env, &args.get(3).unwrap()).unwrap(),
                    amount
                );
            }
            _ => panic!("expected contract function authorization"),
        }

        // The transfer actually happened.
        assert_eq!(token.balance(&from), 10_000 - amount);
        assert_eq!(token.balance(&to), amount);
        assert_eq!(client.get_tip_total(&to), amount);
        assert_eq!(client.get_tip_count(&to), 1);
    }

    #[test]
    #[should_panic(expected = "Tip amount must be positive")]
    fn test_send_tip_rejects_zero_amount() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        env.mock_all_auths();
        client.initialize(&admin);

        let from = Address::generate(&env);
        let to = Address::generate(&env);
        client.send_tip(&Address::generate(&env), &from, &to, &0);
    }

    #[test]
    #[should_panic(expected = "Tip amount must be positive")]
    fn test_send_tip_rejects_negative_amount() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        env.mock_all_auths();
        client.initialize(&admin);

        let from = Address::generate(&env);
        let to = Address::generate(&env);
        client.send_tip(&Address::generate(&env), &from, &to, &-5);
    }

    #[test]
    fn test_send_tip_rejects_unauthorized_caller() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        // Initialize by mocking only the admin's authorization.
        let admin = Address::generate(&env);
        env.mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "initialize",
                args: (&admin,).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        client.initialize(&admin);

        let sac = env.register_stellar_asset_contract_v2(admin.clone());
        let sac_address = sac.address();
        let token = token::Client::new(&env, &sac_address);
        let sac_admin = token::StellarAssetClient::new(&env, &sac_address);

        let from = Address::generate(&env);
        let to = Address::generate(&env);

        // Fund the sender: the SAC only mints for the admin that created it.
        env.mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &sac_address,
                fn_name: "mint",
                args: (&from, &10_000_i128).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        sac_admin.mint(&from, &10_000);
        assert_eq!(token.balance(&from), 10_000);

        // Only `attacker` is authorized for `send_tip`; `from` never signed.
        let attacker = Address::generate(&env);
        env.mock_auths(&[MockAuth {
            address: &attacker,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "send_tip",
                args: (sac_address.clone(), from.clone(), to.clone(), 100_i128).into_val(&env),
                sub_invokes: &[],
            },
        }]);

        let result = client.try_send_tip(&sac_address, &from, &to, &100);
        assert!(result.is_err());

        // Unauthorized call moved no funds and recorded no tip.
        assert_eq!(token.balance(&from), 10_000);
        assert_eq!(token.balance(&to), 0);
        assert_eq!(client.get_tip_total(&to), 0);
        assert_eq!(client.get_tip_count(&to), 0);
    }

    #[test]
    fn test_mint_receipt_requires_from_auth() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        env.mock_all_auths();
        client.initialize(&admin);

        let from = Address::generate(&env);
        let to = Address::generate(&env);

        client.mint_receipt(&from, &to, &1000, &Symbol::new(&env, "Rent"));

        // Exactly one auth entry, owned by the payer, covering the full
        // `mint_receipt` invocation arguments.
        assert_eq!(
            env.auths(),
            [(
                from.clone(),
                AuthorizedInvocation {
                    function: AuthorizedFunction::Contract((
                        client.address.clone(),
                        Symbol::new(&env, "mint_receipt"),
                        (
                            from.clone(),
                            to.clone(),
                            1000_i128,
                            Symbol::new(&env, "Rent")
                        )
                            .into_val(&env)
                    )),
                    sub_invocations: [].into(),
                }
            )]
        );
    }

    /// Issue #1121 acceptance criteria: only the payer can mint their own
    /// receipt — an unrelated authorized address must not mint one on their
    /// behalf, and no receipt state may be written.
    #[test]
    fn test_mint_receipt_rejects_unauthorized_caller() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        env.mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "initialize",
                args: (&admin,).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        client.initialize(&admin);

        let from = Address::generate(&env);
        let to = Address::generate(&env);
        let attacker = Address::generate(&env);

        // Only `attacker` is authorized; the receipt belongs to `from`.
        env.mock_auths(&[MockAuth {
            address: &attacker,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "mint_receipt",
                args: (
                    from.clone(),
                    to.clone(),
                    1000_i128,
                    Symbol::new(&env, "Rent"),
                )
                    .into_val(&env),
                sub_invokes: &[],
            },
        }]);

        let result = client.try_mint_receipt(&from, &to, &1000, &Symbol::new(&env, "Rent"));
        assert!(result.is_err());
        assert_eq!(client.get_receipt_count(&from), 0);
    }

    /// Regression test for the auth-ordering fix: a call that panics on
    /// validation must not leave behind a satisfied authorization for the
    /// payer (which would be usable as a probe inside a bigger transaction).
    #[test]
    fn test_failed_mint_does_not_consume_auth() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        env.mock_all_auths();
        client.initialize(&admin);

        let from = Address::generate(&env);
        let to = Address::generate(&env);

        // Amount validation happens *before* require_auth, so this panics
        // without registering any authorization for `from`.
        let result = client.try_mint_receipt(&from, &to, &0, &Symbol::new(&env, "Bad"));
        assert!(result.is_err());
        assert!(env.auths().is_empty());
        assert_eq!(client.get_receipt_count(&from), 0);

        // A valid call afterwards still works.
        let id = client.mint_receipt(&from, &to, &250, &Symbol::new(&env, "Good"));
        assert_eq!(id, 0);
        assert_eq!(client.get_receipt_count(&from), 1);
    }

    #[test]
    fn test_tip_totals_start_at_zero() {
        let env = Env::default();
        let contract_id = env.register_contract(None, MicroPayContract);
        let client = MicroPayContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        env.mock_all_auths();
        client.initialize(&admin);

        let recipient = Address::generate(&env);
        assert_eq!(client.get_tip_total(&recipient), 0);
        assert_eq!(client.get_tip_count(&recipient), 0);
    }
}
