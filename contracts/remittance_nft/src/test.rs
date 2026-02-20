#![cfg(test)]
use super::*;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

fn create_test_hash(env: &Env, value: u8) -> BytesN<32> {
    let mut hash_bytes = [0u8; 32];
    hash_bytes[0] = value;
    BytesN::from_array(env, &hash_bytes)
}

#[test]
fn test_score_lifecycle() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    
    let contract_id = env.register_contract(None, RemittanceNFT);
    let client = RemittanceNFTClient::new(&env, &contract_id);

    client.initialize(&admin);

    let history_hash = create_test_hash(&env, 1);

    // Initial mint (admin mints, so minter is None)
    client.mint(&user, &500, &history_hash, &None);
    assert_eq!(client.get_score(&user), 500);
    
    // Check metadata
    let metadata = client.get_metadata(&user).unwrap();
    assert_eq!(metadata.score, 500);
    assert_eq!(metadata.history_hash, history_hash);

    // Update score (repayment of 250 -> 2 points) - admin updates
    client.update_score(&user, &250, &None);
    assert_eq!(client.get_score(&user), 502);
    
    // Verify metadata updated
    let metadata = client.get_metadata(&user).unwrap();
    assert_eq!(metadata.score, 502);

    // Update score (repayment of 1000 -> 10 points) - admin updates
    client.update_score(&user, &1000, &None);
    assert_eq!(client.get_score(&user), 512);
    
    // Verify metadata updated
    let metadata = client.get_metadata(&user).unwrap();
    assert_eq!(metadata.score, 512);

    // Unregistered user should have 0 score
    let stranger = Address::generate(&env);
    assert_eq!(client.get_score(&stranger), 0);
    assert!(client.get_metadata(&stranger).is_none());
}

#[test]
fn test_history_hash_update() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    
    let contract_id = env.register_contract(None, RemittanceNFT);
    let client = RemittanceNFTClient::new(&env, &contract_id);

    client.initialize(&admin);

    let initial_hash = create_test_hash(&env, 1);
    client.mint(&user, &500, &initial_hash, &None);
    
    let metadata = client.get_metadata(&user).unwrap();
    assert_eq!(metadata.history_hash, initial_hash);

    // Update history hash - admin updates
    let new_hash = create_test_hash(&env, 2);
    client.update_history_hash(&user, &new_hash, &None);
    
    let metadata = client.get_metadata(&user).unwrap();
    assert_eq!(metadata.history_hash, new_hash);
    assert_eq!(metadata.score, 500); // Score should remain unchanged
}

#[test]
fn test_authorized_minter() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let authorized_contract = Address::generate(&env);
    
    let contract_id = env.register_contract(None, RemittanceNFT);
    let client = RemittanceNFTClient::new(&env, &contract_id);

    client.initialize(&admin);

    // Admin should be authorized by default
    assert!(client.is_authorized_minter(&admin));

    // Authorize a contract
    client.authorize_minter(&authorized_contract);
    assert!(client.is_authorized_minter(&authorized_contract));

    // Revoke authorization
    client.revoke_minter(&authorized_contract);
    assert!(!client.is_authorized_minter(&authorized_contract));
}

#[test]
#[should_panic(expected = "not initialized")]
fn test_not_initialized() {
    let env = Env::default();
    let user = Address::generate(&env);
    let contract_id = env.register_contract(None, RemittanceNFT);
    let client = RemittanceNFTClient::new(&env, &contract_id);

    let history_hash = create_test_hash(&env, 1);
    client.mint(&user, &500, &history_hash, &None);
}

#[test]
#[should_panic(expected = "already initialized")]
fn test_already_initialized() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let contract_id = env.register_contract(None, RemittanceNFT);
    let client = RemittanceNFTClient::new(&env, &contract_id);

    client.initialize(&admin);
    client.initialize(&admin);
}

#[test]
#[should_panic(expected = "user already has an NFT")]
fn test_duplicate_mint() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    
    let contract_id = env.register_contract(None, RemittanceNFT);
    let client = RemittanceNFTClient::new(&env, &contract_id);

    client.initialize(&admin);

    let history_hash = create_test_hash(&env, 1);
    client.mint(&user, &500, &history_hash, &None);
    
    // Try to mint again for the same user
    let history_hash2 = create_test_hash(&env, 2);
    client.mint(&user, &600, &history_hash2, &None);
}

#[test]
#[should_panic(expected = "user does not have an NFT")]
fn test_update_score_without_nft() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    
    let contract_id = env.register_contract(None, RemittanceNFT);
    let client = RemittanceNFTClient::new(&env, &contract_id);

    client.initialize(&admin);

    // Try to update score for user without NFT
    client.update_score(&user, &100, &None);
}
