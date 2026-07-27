use jito_bam_boost_client::instructions::ClaimBuilder;
use solana_pubkey::Pubkey;
use solana_transaction::Instruction;
use spl_associated_token_account_interface::{
    address::get_associated_token_address_with_program_id,
    instruction::create_associated_token_account_idempotent,
};

use crate::{pda, JITOSOL_MINT};

/// Builds the [create ATA, claim] instruction pair for one epoch.
pub fn build_claim_ixs(
    program_id: &Pubkey,
    claimant: &Pubkey,
    epoch: u64,
    amount: u64,
    proof: Vec<[u8; 32]>,
) -> Vec<Instruction> {
    let distributor_pda = pda::merkle_distributor_address(program_id, &JITOSOL_MINT, epoch);
    let distributor_token_address = get_associated_token_address_with_program_id(
        &distributor_pda,
        &JITOSOL_MINT,
        &spl_token_interface::id(),
    );
    let claim_status_pda = pda::claim_status_address(program_id, claimant, &distributor_pda);
    let claimant_token_address = get_associated_token_address_with_program_id(
        claimant,
        &JITOSOL_MINT,
        &spl_token_interface::id(),
    );

    let mut ix_builder = ClaimBuilder::new();
    ix_builder
        .distributor(distributor_pda)
        .claim_status(claim_status_pda)
        .from(distributor_token_address)
        .to(claimant_token_address)
        .claimant(*claimant)
        .token_program(spl_token_interface::id())
        .amount(amount)
        .proof(proof);
    let mut claim_ix = ix_builder.instruction();
    claim_ix.program_id = *program_id;

    vec![
        create_associated_token_account_idempotent(
            claimant,
            claimant,
            &JITOSOL_MINT,
            &spl_token_interface::id(),
        ),
        claim_ix,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_pubkey::Pubkey;
    use std::str::FromStr;

    #[test]
    fn builds_ata_then_claim_instruction() {
        let program_id = Pubkey::from_str("BoostxbPp2ENYHGcTLYt1obpcY13HE4NojdqNWdzqSSb").unwrap();
        let claimant = Pubkey::new_unique();
        let proof = vec![[7u8; 32]];

        let ixs = build_claim_ixs(&program_id, &claimant, 42, 1000, proof);

        assert_eq!(ixs.len(), 2);
        // First: ATA creation for the claimant's JitoSOL account.
        assert_eq!(
            ixs[0].program_id.to_string(),
            "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
        );
        // Second: claim instruction owned by the BAM Boost program.
        assert_eq!(ixs[1].program_id, program_id);
        // Claimant must be a signer of the claim instruction.
        assert!(ixs[1]
            .accounts
            .iter()
            .any(|m| m.pubkey == claimant && m.is_signer));
    }
}
