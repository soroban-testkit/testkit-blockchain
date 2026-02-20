#![cfg(test)]
use super::*;
use soroban_sdk::{testutils::Address as _, Address, Env};

#[test]
fn test_score_lifecycle() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    
    let contract_id = env.register_contract(None, RemittanceNFT);
    let client = RemittanceNFTClient::new(&env, &contract_id);

    client.initialize(&admin);

    // Initial mint
    client.mint(&user, &500);
    assert_eq!(client.get_score(&user), 500);

    // Update score (repayment of 250 -> 2 points)
    client.update_score(&user, &250);
    assert_eq!(client.get_score(&user), 502);

    // Update score (repayment of 1000 -> 10 points)
    client.update_score(&user, &1000);
    assert_eq!(client.get_score(&user), 512);

    // Unregistered user should have 0 score
    let stranger = Address::generate(&env);
    assert_eq!(client.get_score(&stranger), 0);
}

#[test]
#[should_panic(expected = "not initialized")]
fn test_not_initialized() {
    let env = Env::default();
    let user = Address::generate(&env);
    let contract_id = env.register_contract(None, RemittanceNFT);
    let client = RemittanceNFTClient::new(&env, &contract_id);

    client.mint(&user, &500);
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

