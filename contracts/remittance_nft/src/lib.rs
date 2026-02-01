#![no_std]
use soroban_sdk::{contract, contractimpl, symbol_short, vec, Env, Symbol, Vec};

#[contract]
pub struct RemittanceNFT;

#[contractimpl]
impl RemittanceNFT {
    pub fn mint(env: Env, to: Symbol, score: u32) {
        // Mint logic placeholder
    }

    pub fn get_score(env: Env, owner: Symbol) -> u32 {
        750 // Mock score
    }
}
