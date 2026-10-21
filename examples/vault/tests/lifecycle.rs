//! Integration coverage for the vault's authorized deposit and withdrawal
//! lifecycle: balances, emitted events, and the owner-only auth gate.

use soroban_sdk::testutils::{Address as _, Events as _, MockAuth, MockAuthInvoke};
use soroban_sdk::{Address, Env, Event as _, IntoVal};
use vault::{Deposited, Vault, VaultClient, Withdrawn};

fn setup() -> (Env, Address, Address) {
    let env = Env::default();
    let owner = Address::generate(&env);
    let vault_id = env.register(Vault, ());
    VaultClient::new(&env, &vault_id).initialize(&owner);
    (env, owner, vault_id)
}

#[test]
fn authorized_deposit_and_withdrawal_lifecycle_tracks_balance_and_events() {
    let (env, owner, vault_id) = setup();
    let client = VaultClient::new(&env, &vault_id);

    client
        .mock_auths(&[MockAuth {
            address: &owner,
            invoke: &MockAuthInvoke {
                contract: &vault_id,
                fn_name: "deposit_checked",
                args: (400_i128,).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .deposit_checked(&400);
    let deposit_events = env.events().all();
    assert_eq!(client.balance(), 400);
    assert_eq!(
        deposit_events,
        std::vec![Deposited { amount: 400 }.to_xdr(&env, &vault_id)],
    );

    client
        .mock_auths(&[MockAuth {
            address: &owner,
            invoke: &MockAuthInvoke {
                contract: &vault_id,
                fn_name: "withdraw_checked",
                args: (150_i128,).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .withdraw_checked(&150);
    let withdrawal_events = env.events().all();
    assert_eq!(client.balance(), 250);
    assert_eq!(
        withdrawal_events,
        std::vec![Withdrawn { amount: 150 }.to_xdr(&env, &vault_id)],
    );
}

#[test]
fn checked_entry_points_reject_unauthorized_callers() {
    let (env, _owner, vault_id) = setup();
    let client = VaultClient::new(&env, &vault_id);
    let attacker = Address::generate(&env);

    assert!(client.try_deposit_checked(&10).is_err());
    assert!(client.try_withdraw_checked(&10).is_err());
    assert!(client
        .mock_auths(&[MockAuth {
            address: &attacker,
            invoke: &MockAuthInvoke {
                contract: &vault_id,
                fn_name: "deposit_checked",
                args: (10_i128,).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .try_deposit_checked(&10)
        .is_err());
    assert_eq!(client.balance(), 0);
}

#[test]
fn authorized_withdrawal_rejects_an_overdraft() {
    let (env, owner, vault_id) = setup();
    let client = VaultClient::new(&env, &vault_id);

    client
        .mock_auths(&[MockAuth {
            address: &owner,
            invoke: &MockAuthInvoke {
                contract: &vault_id,
                fn_name: "deposit_checked",
                args: (50_i128,).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .deposit_checked(&50);
    assert!(client
        .mock_auths(&[MockAuth {
            address: &owner,
            invoke: &MockAuthInvoke {
                contract: &vault_id,
                fn_name: "withdraw_checked",
                args: (51_i128,).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .try_withdraw_checked(&51)
        .is_err());
    assert_eq!(client.balance(), 50);
}
