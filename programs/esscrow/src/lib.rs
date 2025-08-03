use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::{AssociatedToken},
    token::{self, Token, TokenAccount, Mint, Transfer},
};
use anchor_lang::solana_program::{
    program::invoke,
    system_instruction,
};

declare_id!("7UMWhVX2ZpqLa1iWqUM1tJz6LjRYWQ1oheZpuMtQKxs1");

#[program]
pub mod escrow {
    use super::*;

    // Initialize a new one-way escrow payment and deposit funds in one transaction
    pub fn initialize_escrow(
        ctx: Context<InitializeEscrow>,
        escrow_id: u64,
        amount: u64,
        deal_type: DealType,
        arbiter: Pubkey,
        recipient: Pubkey,
    ) -> Result<()> {
        let escrow = &mut ctx.accounts.escrow;
        
        escrow.escrow_id = escrow_id;
        escrow.initiator = ctx.accounts.initiator.key();
        escrow.recipient = recipient;
        escrow.arbiter = arbiter;
        escrow.amount = amount;
        escrow.released_amount = 0;
        escrow.deal_type = deal_type.clone();
        escrow.status = EscrowStatus::Initialized;
        escrow.bump = ctx.bumps.escrow;
        
        // Deposit funds immediately after initialization
        match deal_type {
            DealType::Sol => {
                // Transfer SOL to escrow vault
                let transfer_instruction = system_instruction::transfer(
                    &ctx.accounts.initiator.key(),
                    &ctx.accounts.escrow_sol_vault.key(),
                    amount,
                );
                
                invoke(
                    &transfer_instruction,
                    &[
                        ctx.accounts.initiator.to_account_info(),
                        ctx.accounts.escrow_sol_vault.to_account_info(),
                        ctx.accounts.system_program.to_account_info(),
                    ],
                )?;
            },
            DealType::Forge => {
                // Transfer FORGE tokens to escrow vault
                let transfer_ctx = CpiContext::new(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.initiator_token_account.to_account_info(),
                        to: ctx.accounts.escrow_token_vault.to_account_info(),
                        authority: ctx.accounts.initiator.to_account_info(),
                    },
                );
                token::transfer(transfer_ctx, amount)?;
            }
        }
        
        // Set status to funded after successful deposit
        escrow.status = EscrowStatus::Funded;
        
        msg!("Escrow initialized and funded with ID: {} (Type: {:?}), Amount: {}", escrow_id, deal_type, amount);
        Ok(())
    }

    // Release funds to recipient with 10% fee - now supports percentage
    pub fn release_funds(
        ctx: Context<ReleaseFunds>,
        percentage: u8, // Percentage to release (1-100)
    ) -> Result<()> {
        let escrow_account_info = ctx.accounts.escrow.to_account_info();
        let escrow = &mut ctx.accounts.escrow;
        
        require!(escrow.status == EscrowStatus::Funded, EscrowError::InvalidEscrowStatus);
        require!(
            ctx.accounts.signer.key() == escrow.arbiter ||
            ctx.accounts.signer.key() == escrow.initiator,
            EscrowError::Unauthorized
        );
        require!(percentage > 0 && percentage <= 100, EscrowError::InvalidPercentage);

        // Calculate amounts based on percentage
        let total_amount = escrow.amount;
        let remaining_amount = total_amount - escrow.released_amount;
        require!(remaining_amount > 0, EscrowError::NoFundsToRelease);

        let release_amount_before_fee = (remaining_amount * percentage as u64) / 100;
        let fee_amount = release_amount_before_fee * 10 / 100; // 10% total fee
        let half_fee = fee_amount / 2; // 5% each for different purposes
        let net_release_amount = release_amount_before_fee - fee_amount;

        let deal_type = escrow.deal_type.clone();
        let escrow_bump = escrow.bump;
        let escrow_id = escrow.escrow_id;

        match deal_type {
            DealType::Sol => {
                // Handle SOL payment
                Escrow::handle_sol_release(
                    ctx.accounts.escrow_sol_vault.to_account_info(),
                    ctx.accounts.recipient.to_account_info(),
                    ctx.accounts.fee_wallet.to_account_info(),
                    ctx.accounts.temp_fee_wallet.to_account_info(),
                    net_release_amount,
                    half_fee,
                )?;
            },
            DealType::Forge => {
                // Handle FORGE token payment
                Escrow::handle_forge_release(
                    ctx.accounts.escrow_token_vault.to_account_info(),
                    ctx.accounts.recipient_token_account.to_account_info(),
                    ctx.accounts.fee_wallet_token_account.to_account_info(),
                    ctx.accounts.burn_token_account.to_account_info(),
                    ctx.accounts.forge_mint.to_account_info(),
                    ctx.accounts.token_program.to_account_info(),
                    escrow_account_info,
                    net_release_amount,
                    half_fee,
                    escrow_bump,
                    escrow_id,
                )?;
            }
        }

        // Update released amount
        escrow.released_amount += release_amount_before_fee;
        
        // Check if fully released
        if escrow.released_amount >= escrow.amount {
            escrow.status = EscrowStatus::Released;
        }
        
        msg!(
            "Partial release ({}%) completed for escrow ID: {}. Released: {}/{}", 
            percentage, 
            escrow_id,
            escrow.released_amount,
            escrow.amount
        );
        Ok(())
    }

    // New function: Get remaining releasable amount
    pub fn get_remaining_amount(ctx: Context<GetRemainingAmount>) -> Result<u64> {
        let escrow = &ctx.accounts.escrow;
        let remaining = escrow.amount - escrow.released_amount;
        msg!("Remaining amount for escrow ID {}: {}", escrow.escrow_id, remaining);
        Ok(remaining)
    }

    // Cancel escrow and return funds to initiator
    pub fn cancel_escrow(ctx: Context<CancelEscrow>) -> Result<()> {
        let escrow_account_info = ctx.accounts.escrow.to_account_info();
        let escrow = &mut ctx.accounts.escrow;
        
        require!(
            escrow.status == EscrowStatus::Funded, // Only funded escrows can be cancelled now
            EscrowError::InvalidEscrowStatus
        );
        require!(
            ctx.accounts.signer.key() == escrow.arbiter ||
            ctx.accounts.signer.key() == escrow.initiator,
            EscrowError::Unauthorized
        );

        let deal_type = escrow.deal_type.clone();
        let remaining_amount = escrow.amount - escrow.released_amount; // Only return unreleased funds
        let escrow_bump = escrow.bump;
        let escrow_id = escrow.escrow_id;

        if remaining_amount > 0 {
            match deal_type {
                DealType::Sol => {
                    // Return remaining SOL to initiator
                    **ctx.accounts.escrow_sol_vault.to_account_info().try_borrow_mut_lamports()? -= remaining_amount;
                    **ctx.accounts.initiator.to_account_info().try_borrow_mut_lamports()? += remaining_amount;
                },
                DealType::Forge => {
                    // Return remaining FORGE tokens to initiator
                    let escrow_id_bytes = escrow_id.to_le_bytes();
                    let seeds = &[
                        b"escrow",
                        escrow_id_bytes.as_ref(),
                        &[escrow_bump]
                    ];
                    let signer = &[&seeds[..]];
                    
                    let transfer_ctx = CpiContext::new_with_signer(
                        ctx.accounts.token_program.to_account_info(),
                        Transfer {
                            from: ctx.accounts.escrow_token_vault.to_account_info(),
                            to: ctx.accounts.initiator_token_account.to_account_info(),
                            authority: escrow_account_info,
                        },
                        signer,
                    );
                    token::transfer(transfer_ctx, remaining_amount)?;
                }
            }
        }

        escrow.status = EscrowStatus::Cancelled;
        msg!("Escrow cancelled for ID: {}", escrow.escrow_id);
        Ok(())
    }
}

impl Escrow {
    fn handle_sol_release(
        escrow_sol_vault: AccountInfo,
        recipient: AccountInfo,
        fee_wallet: AccountInfo,
        temp_fee_wallet: AccountInfo,
        release_amount: u64,
        half_fee: u64,
    ) -> Result<()> {
        // Send release amount to recipient
        **escrow_sol_vault.try_borrow_mut_lamports()? -= release_amount;
        **recipient.try_borrow_mut_lamports()? += release_amount;
        
        // Transfer 5% fee to fee wallet
        **escrow_sol_vault.try_borrow_mut_lamports()? -= half_fee;
        **fee_wallet.try_borrow_mut_lamports()? += half_fee;
        
        // TODO: Buy Forge token from dex by 5% of fee and burn it
        // Note: In production, implement DEX swap for remaining 5%
        // For now, sending remaining fee to temp fee wallet
        **escrow_sol_vault.try_borrow_mut_lamports()? -= half_fee;
        **temp_fee_wallet.try_borrow_mut_lamports()? += half_fee;
        
        Ok(())
    }
    
    fn handle_forge_release<'info>(
        escrow_token_vault: AccountInfo<'info>,
        recipient_token_account: AccountInfo<'info>,
        fee_wallet_token_account: AccountInfo<'info>,
        burn_token_account: AccountInfo<'info>, 
        forge_mint: AccountInfo<'info>,
        token_program: AccountInfo<'info>,
        escrow_authority: AccountInfo<'info>,
        release_amount: u64,
        half_fee: u64,
        bump: u8,
        escrow_id: u64,
    ) -> Result<()> {
        let escrow_id_bytes = escrow_id.to_le_bytes();
        let seeds = &[
            b"escrow",
            escrow_id_bytes.as_ref(),
            &[bump]
        ];
        let signer = &[&seeds[..]];
        
        // Send release amount to recipient
        let transfer_ctx = CpiContext::new_with_signer(
            token_program.clone(),
            Transfer {
                from: escrow_token_vault.clone(),
                to: recipient_token_account,
                authority: escrow_authority.clone(),
            },
            signer,
        );
        token::transfer(transfer_ctx, release_amount)?;
        
        // Transfer 5% fee to fee wallet
        let transfer_ctx = CpiContext::new_with_signer(
            token_program.clone(),
            Transfer {
                from: escrow_token_vault.clone(),
                to: fee_wallet_token_account,
                authority: escrow_authority.clone(),
            },
            signer,
        );
        token::transfer(transfer_ctx, half_fee)?;
        
        // Burn 5% of tokens
        let transfer_ctx = CpiContext::new_with_signer(
            token_program.clone(),
            Transfer {
                from: escrow_token_vault.clone(),
                to: burn_token_account,
                authority: escrow_authority.clone(),
            },
            signer,
        );
        token::transfer(transfer_ctx, half_fee)?;
        
        Ok(())
    }
}

// Account Contexts
#[derive(Accounts)]
#[instruction(escrow_id: u64)]
pub struct InitializeEscrow<'info> {
    #[account(
        init,
        payer = initiator,
        space = 8 + Escrow::INIT_SPACE,
        seeds = [b"escrow", escrow_id.to_le_bytes().as_ref()],
        bump
    )]
    pub escrow: Account<'info, Escrow>,
    
    #[account(mut)]
    pub initiator: Signer<'info>,
    
    /// CHECK: This is safe because we're only using it as a vault
    #[account(
        init,
        payer = initiator,
        space = 0,
        seeds = [b"sol_vault", escrow_id.to_le_bytes().as_ref()],
        bump
    )]
    pub escrow_sol_vault: AccountInfo<'info>,
    
    #[account(
        init_if_needed,
        payer = initiator,
        associated_token::mint = forge_mint,
        associated_token::authority = escrow
    )]
    pub escrow_token_vault: Account<'info, TokenAccount>,
    
    // Add initiator's token account for FORGE transfers
    #[account(mut)]
    pub initiator_token_account: Account<'info, TokenAccount>,
    
    pub forge_mint: Account<'info, Mint>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
}

#[derive(Accounts)]
pub struct ReleaseFunds<'info> {
    #[account(mut)]
    pub escrow: Account<'info, Escrow>,
    
    pub signer: Signer<'info>,
    
    /// CHECK: Safe for SOL operations
    #[account(mut)]
    pub escrow_sol_vault: AccountInfo<'info>,
    
    /// CHECK: Safe for SOL operations
    #[account(mut)]
    pub recipient: AccountInfo<'info>,
    
    /// CHECK: Safe for SOL operations
    #[account(mut)]
    pub fee_wallet: AccountInfo<'info>,

    /// CHECK: Safe for SOL operations
    #[account(mut)]
    pub temp_fee_wallet: AccountInfo<'info>,
    
    #[account(mut)]
    pub escrow_token_vault: Account<'info, TokenAccount>,
    
    #[account(mut)]
    pub recipient_token_account: Account<'info, TokenAccount>,
    
    #[account(mut)]
    pub fee_wallet_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub burn_token_account: Account<'info, TokenAccount>,
    
    #[account(mut)]
    pub forge_mint: Account<'info, Mint>,
    
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct GetRemainingAmount<'info> {
    pub escrow: Account<'info, Escrow>,
}

#[derive(Accounts)]
pub struct CancelEscrow<'info> {
    #[account(mut)]
    pub escrow: Account<'info, Escrow>,
    
    pub signer: Signer<'info>,
    
    /// CHECK: Safe for SOL operations
    #[account(mut)]
    pub escrow_sol_vault: AccountInfo<'info>,
    
    /// CHECK: Safe for SOL operations
    #[account(mut)]
    pub initiator: AccountInfo<'info>,
    
    #[account(mut)]
    pub escrow_token_vault: Account<'info, TokenAccount>,
    
    #[account(mut)]
    pub initiator_token_account: Account<'info, TokenAccount>,
    
    pub token_program: Program<'info, Token>,
}

// Data Structures
#[account]
#[derive(InitSpace)]
pub struct Escrow {
    pub escrow_id: u64,
    pub initiator: Pubkey,      // Person paying (Alice)
    pub recipient: Pubkey,      // Person receiving payment (Bob)
    pub arbiter: Pubkey,        // Third party who can resolve disputes
    pub amount: u64,            // Total amount to be paid
    pub released_amount: u64,   // Amount already released
    pub deal_type: DealType,    // SOL or FORGE tokens
    pub status: EscrowStatus,   // Current status
    pub bump: u8,               // PDA bump
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Eq, InitSpace, Debug)]
pub enum DealType {
    Sol,    // One-way SOL payment
    Forge,  // One-way FORGE token payment
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Eq, InitSpace)]
pub enum EscrowStatus {
    Initialized,  // Escrow created, waiting for deposit (not used anymore)
    Funded,       // Funds deposited, waiting for release
    Released,     // All funds released to recipient
    Cancelled,    // Escrow cancelled, funds returned to initiator
}

// Errors
#[error_code]
pub enum EscrowError {
    #[msg("Invalid escrow status for this operation")]
    InvalidEscrowStatus,
    #[msg("Unauthorized to perform this action")]
    Unauthorized,
    #[msg("Insufficient funds")]
    InsufficientFunds,
    #[msg("Invalid deal type")]
    InvalidDealType,
    #[msg("Only the initiator can deposit for payments")]
    OnlyInitiatorCanDeposit,
    #[msg("Invalid percentage: must be between 1 and 100")]
    InvalidPercentage,
    #[msg("No funds remaining to release")]
    NoFundsToRelease,
    #[msg("Invalid Burn Address")]
    InvalidBurnAddress
}