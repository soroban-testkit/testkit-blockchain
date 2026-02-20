#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};


#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Score(Address),
    Admin,
}

#[contract]
pub struct RemittanceNFT;

#[contractimpl]
impl RemittanceNFT {
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
    }

    pub fn mint(env: Env, user: Address, initial_score: u32) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).expect("not initialized");
        admin.require_auth();

        let key = DataKey::Score(user.clone());
        if env.storage().persistent().has(&key) {
            panic!("user already has a score");
        }
        env.storage().persistent().set(&key, &initial_score);
    }

    pub fn get_score(env: Env, user: Address) -> u32 {
        let key = DataKey::Score(user);
        env.storage().persistent().get(&key).unwrap_or(0)
    }

    pub fn update_score(env: Env, user: Address, repayment_amount: i128) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).expect("not initialized");
        admin.require_auth();

        let key = DataKey::Score(user.clone());
        let mut score: u32 = env.storage().persistent().get(&key).unwrap_or(0);
        
        // Simple logic: 1 point per 100 units of repayment
        let points = (repayment_amount / 100) as u32;
        score += points;

        env.storage().persistent().set(&key, &score);
    }
}

mod test;


