#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, Address, BytesN, Env};


#[contracttype]
#[derive(Clone)]
pub struct RemittanceMetadata {
    pub score: u32,
    pub history_hash: BytesN<32>,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Metadata(Address),
    Admin,
    AuthorizedMinter(Address),
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
        // Admin is automatically authorized to mint
        env.storage().instance().set(&DataKey::AuthorizedMinter(admin.clone()), &true);
    }

    /// Authorize a contract or account to mint NFTs
    pub fn authorize_minter(env: Env, minter: Address) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).expect("not initialized");
        admin.require_auth();
        
        env.storage().instance().set(&DataKey::AuthorizedMinter(minter), &true);
    }

    /// Revoke authorization for a contract or account to mint NFTs
    pub fn revoke_minter(env: Env, minter: Address) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).expect("not initialized");
        admin.require_auth();
        
        env.storage().instance().remove(&DataKey::AuthorizedMinter(minter));
    }

    /// Check if an address is authorized to mint
    pub fn is_authorized_minter(env: Env, minter: Address) -> bool {
        env.storage().instance().get(&DataKey::AuthorizedMinter(minter)).unwrap_or(false)
    }

    /// Mint an NFT representing a user's remittance history and reputation score
    /// Only authorized contracts/accounts can call this function
    /// If minter is provided, it must be authorized and must authorize the call
    /// If minter is None, admin must authorize the call
    pub fn mint(env: Env, user: Address, initial_score: u32, history_hash: BytesN<32>, minter: Option<Address>) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).expect("not initialized");
        
        if let Some(minter_addr) = minter {
            // If minter is provided, require their auth and check authorization
            minter_addr.require_auth();
            let is_authorized = env.storage().instance().get(&DataKey::AuthorizedMinter(minter_addr)).unwrap_or(false);
            if !is_authorized {
                panic!("minter is not authorized");
            }
        } else {
            // If no minter provided, require admin auth
            admin.require_auth();
        }

        let key = DataKey::Metadata(user.clone());
        if env.storage().persistent().has(&key) {
            panic!("user already has an NFT");
        }

        let metadata = RemittanceMetadata {
            score: initial_score,
            history_hash,
        };
        
        env.storage().persistent().set(&key, &metadata);
    }

    /// Get the metadata (score and history hash) for a user's NFT
    pub fn get_metadata(env: Env, user: Address) -> Option<RemittanceMetadata> {
        let key = DataKey::Metadata(user);
        env.storage().persistent().get(&key)
    }

    /// Get the score for a user (backward compatibility)
    pub fn get_score(env: Env, user: Address) -> u32 {
        let key = DataKey::Metadata(user);
        if let Some(metadata) = env.storage().persistent().get::<DataKey, RemittanceMetadata>(&key) {
            metadata.score
        } else {
            0
        }
    }

    /// Update the score for a user's NFT
    /// Only authorized contracts/accounts can call this function
    /// If minter is provided, it must be authorized and must authorize the call
    /// If minter is None, admin must authorize the call
    pub fn update_score(env: Env, user: Address, repayment_amount: i128, minter: Option<Address>) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).expect("not initialized");
        
        if let Some(minter_addr) = minter {
            // If minter is provided, require their auth and check authorization
            minter_addr.require_auth();
            let is_authorized = env.storage().instance().get(&DataKey::AuthorizedMinter(minter_addr)).unwrap_or(false);
            if !is_authorized {
                panic!("minter is not authorized");
            }
        } else {
            // If no minter provided, require admin auth
            admin.require_auth();
        }

        let key = DataKey::Metadata(user.clone());
        let mut metadata: RemittanceMetadata = env.storage().persistent().get(&key).expect("user does not have an NFT");
        
        // Simple logic: 1 point per 100 units of repayment
        let points = (repayment_amount / 100) as u32;
        metadata.score += points;

        env.storage().persistent().set(&key, &metadata);
    }

    /// Update the history hash for a user's NFT
    /// Only authorized contracts/accounts can call this function
    /// If minter is provided, it must be authorized and must authorize the call
    /// If minter is None, admin must authorize the call
    pub fn update_history_hash(env: Env, user: Address, new_history_hash: BytesN<32>, minter: Option<Address>) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).expect("not initialized");
        
        if let Some(minter_addr) = minter {
            // If minter is provided, require their auth and check authorization
            minter_addr.require_auth();
            let is_authorized = env.storage().instance().get(&DataKey::AuthorizedMinter(minter_addr)).unwrap_or(false);
            if !is_authorized {
                panic!("minter is not authorized");
            }
        } else {
            // If no minter provided, require admin auth
            admin.require_auth();
        }

        let key = DataKey::Metadata(user.clone());
        let mut metadata: RemittanceMetadata = env.storage().persistent().get(&key).expect("user does not have an NFT");
        
        metadata.history_hash = new_history_hash;

        env.storage().persistent().set(&key, &metadata);
    }
}

mod test;


