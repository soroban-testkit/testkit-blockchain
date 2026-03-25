#![no_std]
use soroban_sdk::{
    contract, contractclient, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
    String, Vec,
};

#[contractclient(name = "NftClient")]
pub trait RemittanceNftInterface {
    fn get_score(env: Env, user: Address) -> u32;
    fn update_score(env: Env, user: Address, repayment_amount: i128, minter: Option<Address>);
    fn seize_collateral(env: Env, user: Address, minter: Option<Address>);
    fn is_seized(env: Env, user: Address) -> bool;
}

mod events;

#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum LoanStatus {
    Pending,
    Approved,
    Repaid,
    Defaulted,
    Cancelled,
    Rejected,
}

#[contracttype]
#[derive(Clone)]
pub struct Loan {
    pub borrower: Address,
    pub amount: i128,
    pub principal_paid: i128,
    pub interest_paid: i128,
    pub accrued_interest: i128,
    pub interest_rate_bps: u32,
    pub due_date: u32,
    pub last_interest_ledger: u32,
    pub status: LoanStatus,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    NftContract,
    LendingPool,
    Token,
    Admin,
    Loan(u32),
    LoanCounter,
    MinScore,
    Paused,
    InterestRateBps,
    DefaultTermLedgers,
    Version,
}

#[contract]
pub struct LoanManager;

#[contractimpl]
impl LoanManager {
    const INSTANCE_TTL_THRESHOLD: u32 = 17280;
    const INSTANCE_TTL_BUMP: u32 = 518400;
    const PERSISTENT_TTL_THRESHOLD: u32 = 17280;
    const PERSISTENT_TTL_BUMP: u32 = 518400;
    const DEFAULT_INTEREST_RATE_BPS: u32 = 1200;
    const DEFAULT_TERM_LEDGERS: u32 = 17280;
    const CURRENT_VERSION: u32 = 1;

    fn bump_instance_ttl(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_BUMP);
    }

    fn bump_persistent_ttl(env: &Env, key: &DataKey) {
        env.storage().persistent().extend_ttl(
            key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_BUMP,
        );
    }

    fn nft_contract(env: &Env) -> Address {
        Self::bump_instance_ttl(env);
        env.storage()
            .instance()
            .get(&DataKey::NftContract)
            .expect("not initialized")
    }

    fn admin(env: &Env) -> Address {
        Self::bump_instance_ttl(env);
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized")
    }

    fn read_interest_rate(env: &Env) -> u32 {
        Self::bump_instance_ttl(env);
        env.storage()
            .instance()
            .get(&DataKey::InterestRateBps)
            .unwrap_or(Self::DEFAULT_INTEREST_RATE_BPS)
    }

    fn read_default_term(env: &Env) -> u32 {
        Self::bump_instance_ttl(env);
        env.storage()
            .instance()
            .get(&DataKey::DefaultTermLedgers)
            .unwrap_or(Self::DEFAULT_TERM_LEDGERS)
    }

    fn assert_not_paused(env: &Env) {
        Self::bump_instance_ttl(env);
        let paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if paused {
            panic!("contract is paused");
        }
    }

    fn remaining_principal(loan: &Loan) -> i128 {
        loan.amount
            .checked_sub(loan.principal_paid)
            .expect("principal paid exceeds amount")
    }

    fn accrue_interest(env: &Env, loan: &mut Loan) {
        if loan.status != LoanStatus::Approved {
            return;
        }

        let current_ledger = env.ledger().sequence();
        if loan.last_interest_ledger == 0 || current_ledger <= loan.last_interest_ledger {
            return;
        }

        let remaining_principal = Self::remaining_principal(loan);
        if remaining_principal <= 0 {
            loan.last_interest_ledger = current_ledger;
            return;
        }

        let elapsed_ledgers = current_ledger - loan.last_interest_ledger;
        let interest_delta = remaining_principal
            .checked_mul(loan.interest_rate_bps as i128)
            .and_then(|value| value.checked_mul(elapsed_ledgers as i128))
            .and_then(|value| value.checked_div(10_000))
            .and_then(|value| value.checked_div(Self::DEFAULT_TERM_LEDGERS as i128))
            .expect("interest overflow");

        loan.accrued_interest = loan
            .accrued_interest
            .checked_add(interest_delta)
            .expect("interest overflow");
        loan.last_interest_ledger = current_ledger;
    }

    fn current_total_debt(env: &Env, loan: &mut Loan) -> i128 {
        Self::accrue_interest(env, loan);
        Self::remaining_principal(loan)
            .checked_add(loan.accrued_interest)
            .expect("debt overflow")
    }

    pub fn initialize(
        env: Env,
        nft_contract: Address,
        lending_pool: Address,
        token: Address,
        admin: Address,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        env.storage()
            .instance()
            .set(&DataKey::NftContract, &nft_contract);
        env.storage()
            .instance()
            .set(&DataKey::LendingPool, &lending_pool);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::LoanCounter, &0u32);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage()
            .instance()
            .set(&DataKey::Version, &Self::CURRENT_VERSION);
        Self::bump_instance_ttl(&env);
    }

    pub fn version(env: Env) -> u32 {
        Self::bump_instance_ttl(&env);
        env.storage().instance().get(&DataKey::Version).unwrap_or(0)
    }

    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        Self::admin(&env).require_auth();
        env.deployer().update_current_contract_wasm(new_wasm_hash);
    }

    pub fn migrate(env: Env) {
        Self::admin(&env).require_auth();

        let current_version: u32 = env.storage().instance().get(&DataKey::Version).unwrap_or(0);

        if current_version < Self::CURRENT_VERSION {
            env.storage()
                .instance()
                .set(&DataKey::Version, &Self::CURRENT_VERSION);
        }

        Self::bump_instance_ttl(&env);
    }

    pub fn request_loan(env: Env, borrower: Address, amount: i128) -> u32 {
        borrower.require_auth();
        Self::assert_not_paused(&env);

        if amount <= 0 {
            panic!("loan amount must be positive");
        }

        let nft_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::NftContract)
            .expect("not initialized");
        let nft_client = NftClient::new(&env, &nft_contract);

        let score = nft_client.get_score(&borrower);
        let min_score: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MinScore)
            .unwrap_or(500);
        if score < min_score {
            panic!("score too low for loan");
        }

        // Create loan record
        let mut loan_counter: u32 = env
            .storage()
            .instance()
            .get(&DataKey::LoanCounter)
            .unwrap_or(0);
        loan_counter += 1;

        let loan = Loan {
            borrower: borrower.clone(),
            amount,
            principal_paid: 0,
            interest_paid: 0,
            accrued_interest: 0,
            interest_rate_bps: Self::read_interest_rate(&env),
            due_date: 0,
            last_interest_ledger: 0,
            status: LoanStatus::Pending,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Loan(loan_counter), &loan);
        env.storage()
            .instance()
            .set(&DataKey::LoanCounter, &loan_counter);
        Self::bump_instance_ttl(&env);
        Self::bump_persistent_ttl(&env, &DataKey::Loan(loan_counter));

        events::loan_requested(&env, borrower.clone(), amount);
        env.events()
            .publish((symbol_short!("LoanReq"), borrower), loan_counter);

        loan_counter
    }

    pub fn approve_loan(env: Env, loan_id: u32) {
        use soroban_sdk::token::TokenClient;

        // Access control: only admin can approve loans
        Self::admin(&env).require_auth();
        Self::assert_not_paused(&env);

        // Get loan record
        let loan_key = DataKey::Loan(loan_id);
        let mut loan: Loan = env
            .storage()
            .persistent()
            .get(&loan_key)
            .expect("loan not found");
        Self::bump_persistent_ttl(&env, &loan_key);

        // Check loan status
        if loan.status != LoanStatus::Pending {
            panic!("loan is not pending");
        }

        // Update loan status to Approved
        loan.status = LoanStatus::Approved;
        loan.due_date = env.ledger().sequence() + Self::read_default_term(&env);
        loan.last_interest_ledger = env.ledger().sequence();
        env.storage().persistent().set(&loan_key, &loan);
        Self::bump_persistent_ttl(&env, &loan_key);

        // Transfer funds from LendingPool to borrower
        let lending_pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::LendingPool)
            .expect("lending pool not set");
        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .expect("token not set");
        let token_client = TokenClient::new(&env, &token);
        token_client.transfer(&lending_pool, &loan.borrower, &loan.amount);

        events::loan_approved(&env, loan_id);
        env.events()
            .publish((symbol_short!("LoanAppr"), loan.borrower.clone()), loan_id);
    }

    pub fn get_loan(env: Env, loan_id: u32) -> Loan {
        let loan_key = DataKey::Loan(loan_id);
        let mut loan = env
            .storage()
            .persistent()
            .get(&loan_key)
            .expect("loan not found");
        Self::bump_persistent_ttl(&env, &loan_key);
        Self::accrue_interest(&env, &mut loan);
        loan
    }

    pub fn repay(env: Env, borrower: Address, loan_id: u32, amount: i128) {
        use soroban_sdk::token::TokenClient;

        borrower.require_auth();
        Self::assert_not_paused(&env);

        if amount <= 0 {
            panic!("repayment amount must be positive");
        }

        let loan_key = DataKey::Loan(loan_id);
        let mut loan: Loan = env
            .storage()
            .persistent()
            .get(&loan_key)
            .expect("loan not found");

        if loan.borrower != borrower {
            panic!("borrower does not own loan");
        }
        if loan.status != LoanStatus::Approved {
            panic!("loan is not active");
        }

        let current_ledger = env.ledger().sequence();
        if current_ledger > loan.due_date {
            panic!("loan is past due");
        }

        let total_debt = Self::current_total_debt(&env, &mut loan);
        if amount > total_debt {
            panic!("repayment exceeds current total debt");
        }

        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .expect("token not set");
        let lending_pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::LendingPool)
            .expect("lending pool not set");
        let token_client = TokenClient::new(&env, &token);
        token_client.transfer(&borrower, &lending_pool, &amount);

        let interest_payment = amount.min(loan.accrued_interest);
        let principal_payment = amount
            .checked_sub(interest_payment)
            .expect("repayment underflow");

        loan.interest_paid = loan
            .interest_paid
            .checked_add(interest_payment)
            .expect("interest paid overflow");
        loan.accrued_interest = loan
            .accrued_interest
            .checked_sub(interest_payment)
            .expect("interest underflow");
        loan.principal_paid = loan
            .principal_paid
            .checked_add(principal_payment)
            .expect("principal paid overflow");

        if loan.principal_paid == loan.amount && loan.accrued_interest == 0 {
            loan.status = LoanStatus::Repaid;
        }

        env.storage().persistent().set(&loan_key, &loan);

        // Skip cross-contract call when repayment rounds to zero score points.
        if amount >= 100 {
            let nft_contract = Self::nft_contract(&env);
            let nft_client = NftClient::new(&env, &nft_contract);
            nft_client.update_score(&borrower, &amount, &None);
        }

        events::loan_repaid(&env, borrower, loan_id, amount);
    }

    pub fn cancel_loan(env: Env, borrower: Address, loan_id: u32) {
        borrower.require_auth();
        Self::assert_not_paused(&env);

        let loan_key = DataKey::Loan(loan_id);
        let mut loan: Loan = env
            .storage()
            .persistent()
            .get(&loan_key)
            .expect("loan not found");
        Self::bump_persistent_ttl(&env, &loan_key);

        if loan.borrower != borrower {
            panic!("borrower does not own loan");
        }
        if loan.status != LoanStatus::Pending {
            panic!("loan is not pending");
        }

        loan.status = LoanStatus::Cancelled;
        env.storage().persistent().set(&loan_key, &loan);
        Self::bump_persistent_ttl(&env, &loan_key);
        events::loan_cancelled(&env, borrower, loan_id);
    }

    pub fn reject_loan(env: Env, loan_id: u32, reason: String) {
        Self::admin(&env).require_auth();
        Self::assert_not_paused(&env);

        let loan_key = DataKey::Loan(loan_id);
        let mut loan: Loan = env
            .storage()
            .persistent()
            .get(&loan_key)
            .expect("loan not found");
        Self::bump_persistent_ttl(&env, &loan_key);

        if loan.status != LoanStatus::Pending {
            panic!("loan is not pending");
        }

        loan.status = LoanStatus::Rejected;
        env.storage().persistent().set(&loan_key, &loan);
        Self::bump_persistent_ttl(&env, &loan_key);
        events::loan_rejected(&env, loan_id, reason);
    }

    pub fn set_min_score(env: Env, min_score: u32) {
        Self::admin(&env).require_auth();

        let old_score: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MinScore)
            .unwrap_or(500);
        env.storage().instance().set(&DataKey::MinScore, &min_score);
        Self::bump_instance_ttl(&env);
        events::min_score_updated(&env, old_score, min_score);
    }

    pub fn get_min_score(env: Env) -> u32 {
        Self::bump_instance_ttl(&env);
        env.storage()
            .instance()
            .get(&DataKey::MinScore)
            .unwrap_or(500)
    }

    pub fn set_interest_rate(env: Env, rate_bps: u32) {
        Self::admin(&env).require_auth();
        if rate_bps == 0 {
            panic!("interest rate must be positive");
        }

        let old_rate = Self::read_interest_rate(&env);
        env.storage()
            .instance()
            .set(&DataKey::InterestRateBps, &rate_bps);
        Self::bump_instance_ttl(&env);
        events::interest_rate_updated(&env, old_rate, rate_bps);
    }

    pub fn get_interest_rate(env: Env) -> u32 {
        Self::read_interest_rate(&env)
    }

    pub fn set_default_term(env: Env, ledgers: u32) {
        Self::admin(&env).require_auth();
        if ledgers == 0 {
            panic!("default term must be positive");
        }

        let old_term = Self::read_default_term(&env);
        env.storage()
            .instance()
            .set(&DataKey::DefaultTermLedgers, &ledgers);
        Self::bump_instance_ttl(&env);
        events::default_term_updated(&env, old_term, ledgers);
    }

    pub fn get_default_term(env: Env) -> u32 {
        Self::read_default_term(&env)
    }

    pub fn pause(env: Env) {
        Self::admin(&env).require_auth();

        env.storage().instance().set(&DataKey::Paused, &true);
        Self::bump_instance_ttl(&env);
        events::paused(&env);
    }

    pub fn unpause(env: Env) {
        Self::admin(&env).require_auth();

        env.storage().instance().set(&DataKey::Paused, &false);
        Self::bump_instance_ttl(&env);
        events::unpaused(&env);
    }

    pub fn check_default(env: Env, loan_id: u32) {
        Self::admin(&env).require_auth();
        Self::assert_not_paused(&env);

        let loan_key = DataKey::Loan(loan_id);
        let mut loan: Loan = env
            .storage()
            .persistent()
            .get(&loan_key)
            .expect("loan not found");
        Self::bump_persistent_ttl(&env, &loan_key);

        if loan.status != LoanStatus::Approved {
            panic!("loan is not active");
        }

        let current_ledger = env.ledger().sequence();
        if current_ledger <= loan.due_date {
            panic!("loan is not past due");
        }

        loan.status = LoanStatus::Defaulted;
        env.storage().persistent().set(&loan_key, &loan);
        Self::bump_persistent_ttl(&env, &loan_key);

        let nft_contract = Self::nft_contract(&env);
        let nft_client = NftClient::new(&env, &nft_contract);
        nft_client.seize_collateral(&loan.borrower, &None);

        events::loan_defaulted(&env, loan_id, loan.borrower.clone());
    }

    pub fn check_defaults(env: Env, loan_ids: Vec<u32>) {
        Self::admin(&env).require_auth();
        Self::assert_not_paused(&env);

        for loan_id in loan_ids.iter() {
            let loan_key = DataKey::Loan(loan_id);
            let mut loan: Loan = match env.storage().persistent().get(&loan_key) {
                Some(l) => l,
                None => continue,
            };
            Self::bump_persistent_ttl(&env, &loan_key);

            if loan.status != LoanStatus::Approved {
                continue;
            }

            let current_ledger = env.ledger().sequence();
            if current_ledger <= loan.due_date {
                continue;
            }

            loan.status = LoanStatus::Defaulted;
            env.storage().persistent().set(&loan_key, &loan);
            Self::bump_persistent_ttl(&env, &loan_key);

            let nft_contract = Self::nft_contract(&env);
            let nft_client = NftClient::new(&env, &nft_contract);
            nft_client.seize_collateral(&loan.borrower, &None);

            events::loan_defaulted(&env, loan_id, loan.borrower.clone());
        }
    }
}

#[cfg(test)]
mod test;