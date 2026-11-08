//! STAG — a NEP-141 memecoin whose supply follows a repeated stag hunt.
//!
//! Time is cut into rounds of `round_secs`. During a round any holder can lock
//! ("hunt with") some of their STAG. When the round closes:
//!
//! * the hunt **succeeds** if the locked pot is at least `target_bps` of the whole
//!   STAG supply and at least `min_hunters` accounts joined. Every hunter gets their
//!   stake back plus `reward_bps` of it, newly minted. The target rises by
//!   `target_step_up_bps` for the next round.
//! * otherwise the hunt **fails**. Every hunter gets their stake back minus
//!   `burn_bps` of it, which is burned. The target falls by `target_step_down_bps`.
//! * a round nobody joined counts as empty: nothing is minted or burned and the
//!   target falls by `target_step_down_bps`.
//!
//! All parameters are fixed in `new` and readable with `get_config`. There is no
//! owner and no method that changes them.
//!
//! Settlement is lazy: the first state-changing call after a round closes settles
//! it before doing anything else, so the result only depends on what happened
//! inside the round. View methods compute the same settlement on the fly, so
//! balances and supply are correct the moment a round closes, even if nobody has
//! sent a transaction since.

use near_contract_standards::fungible_token::events::{FtBurn, FtMint, FtTransfer};
use near_contract_standards::fungible_token::metadata::{
    FT_METADATA_SPEC, FungibleTokenMetadata, FungibleTokenMetadataProvider,
};
use near_contract_standards::fungible_token::{
    Balance, FungibleToken, FungibleTokenCore, FungibleTokenResolver,
};
use near_contract_standards::storage_management::{
    StorageBalance, StorageBalanceBounds, StorageManagement,
};
use near_sdk::json_types::U128;
use near_sdk::serde_json::json;
use near_sdk::store::{LookupMap, LookupSet, Vector};
use near_sdk::{
    AccountId, BorshStorageKey, NearToken, PanicOnDefault, Promise, PromiseOrValue,
    assert_one_yocto, env, log, near, require,
};

pub const BPS: u128 = 10_000;
const NS_PER_SEC: u64 = 1_000_000_000;
const NS_PER_MS: u64 = 1_000_000;
const MAX_PAGE: u64 = 50;

const ICON: &str = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 64 64'%3E%3Ccircle cx='32' cy='32' r='30' fill='%23263a2f'/%3E%3Cpath d='M20 14l4 10-6-2 8 8M44 14l-4 10 6-2-8 8M24 30c0-4 4-6 8-6s8 2 8 6l-2 14c-1 4-4 6-6 6s-5-2-6-6z' fill='none' stroke='%23e9d8a6' stroke-width='3' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E";

#[derive(BorshStorageKey)]
#[near(serializers = [borsh])]
enum StorageKey {
    Accounts,
    Stakes,
    Results,
    History,
    FaucetClaimed,
}

/// Launch parameters. Fixed forever at `new`.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Length of one hunt, in seconds.
    pub round_secs: u64,
    /// Minted on top of each hunter's stake when a hunt succeeds (basis points).
    pub reward_bps: u16,
    /// Burned from each hunter's stake when a hunt fails (basis points).
    pub burn_bps: u16,
    /// Share of total supply the pot must reach in the first round (basis points).
    pub start_target_bps: u16,
    /// Target change after a successful hunt.
    pub target_step_up_bps: u16,
    /// Target change after a failed or empty hunt.
    pub target_step_down_bps: u16,
    pub min_target_bps: u16,
    pub max_target_bps: u16,
    /// A hunt with fewer distinct accounts fails even if the pot is large enough.
    pub min_hunters: u32,
    /// STAG (in base units) the starter faucet mints once per account.
    pub faucet_amount: U128,
    /// The faucet stops minting at this time (Unix ms).
    pub faucet_closes_at_ms: u64,
}

impl Config {
    fn assert_valid(&self) {
        require!(self.round_secs > 0, "round_secs must be positive");
        require!((self.reward_bps as u128) <= BPS, "reward_bps must be <= 10000");
        require!((self.burn_bps as u128) <= BPS, "burn_bps must be <= 10000");
        require!(self.min_target_bps > 0, "min_target_bps must be positive");
        require!(
            self.min_target_bps <= self.start_target_bps
                && self.start_target_bps <= self.max_target_bps,
            "need min_target_bps <= start_target_bps <= max_target_bps"
        );
        require!((self.max_target_bps as u128) <= BPS, "max_target_bps must be <= 10000");
        require!(self.min_hunters >= 1, "min_hunters must be at least 1");
    }

    pub fn bonus(&self, stake: Balance) -> Balance {
        stake * self.reward_bps as u128 / BPS
    }

    pub fn burn(&self, stake: Balance) -> Balance {
        stake * self.burn_bps as u128 / BPS
    }

    fn step_up(&self, bps: u16) -> u16 {
        (bps as u32 + self.target_step_up_bps as u32).min(self.max_target_bps as u32) as u16
    }

    fn step_down(&self, bps: u16, times: u64) -> u16 {
        let drop = (self.target_step_down_bps as u64).saturating_mul(times);
        (bps as u64).saturating_sub(drop).max(self.min_target_bps as u64) as u16
    }
}

/// Pot needed for a hunt to succeed: ceil(target_bps * supply / 10000).
pub fn target_amount(target_bps: u16, supply: Balance) -> Balance {
    (supply * target_bps as u128).div_ceil(BPS)
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct OpenRound {
    pub id: u64,
    pub pot: Balance,
    pub hunters: u32,
    /// Sum of each hunter's bonus if this round succeeds (exact, per-account rounding).
    pub win_delta: Balance,
    /// Sum of each hunter's burn if this round fails (exact, per-account rounding).
    pub lose_delta: Balance,
    pub target_bps: u16,
}

/// Small state that settlement rewrites.
#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct Ledger {
    pub open: OpenRound,
    /// Bonuses of settled, won rounds that hunters have not collected yet.
    pub pending_mint: Balance,
    /// Burns of settled, lost rounds that hunters have not collected yet.
    pub pending_burn: Balance,
    /// Stakes of settled rounds still held by the contract account.
    pub escrow_settled: Balance,
    pub streak: u32,
    pub best_streak: u32,
    pub rounds_won: u64,
    pub rounds_lost: u64,
    pub rounds_empty: u64,
    pub total_minted: Balance,
    pub total_burned: Balance,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct RoundResult {
    pub round: u64,
    pub pot: Balance,
    pub hunters: u32,
    pub target_bps: u16,
    pub target: Balance,
    pub supply_at_close: Balance,
    pub won: bool,
    /// Minted if won, burned if lost.
    pub delta: Balance,
    pub next_target_bps: u16,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct Stake {
    pub round: u64,
    pub amount: Balance,
}

/// Settle every round that closed before `now_round`. Pure: the same function backs
/// the state-changing path and the view path.
pub fn settle_ledger(
    cfg: &Config,
    ledger: &Ledger,
    stored_supply: Balance,
    now_round: u64,
) -> (Ledger, Option<RoundResult>) {
    let mut l = ledger.clone();
    if l.open.id >= now_round {
        return (l, None);
    }
    let o = l.open.clone();
    let supply = stored_supply + l.pending_mint - l.pending_burn;
    let mut result = None;
    let mut next_bps;
    if o.pot > 0 {
        let target = target_amount(o.target_bps, supply);
        let won = o.hunters >= cfg.min_hunters && o.pot >= target;
        let delta;
        if won {
            delta = o.win_delta;
            l.pending_mint += delta;
            l.total_minted += delta;
            l.rounds_won += 1;
            l.streak += 1;
            l.best_streak = l.best_streak.max(l.streak);
            next_bps = cfg.step_up(o.target_bps);
        } else {
            delta = o.lose_delta;
            l.pending_burn += delta;
            l.total_burned += delta;
            l.rounds_lost += 1;
            l.streak = 0;
            next_bps = cfg.step_down(o.target_bps, 1);
        }
        l.escrow_settled += o.pot;
        result = Some(RoundResult {
            round: o.id,
            pot: o.pot,
            hunters: o.hunters,
            target_bps: o.target_bps,
            target,
            supply_at_close: supply,
            won,
            delta,
            next_target_bps: next_bps,
        });
    } else {
        l.rounds_empty += 1;
        l.streak = 0;
        next_bps = cfg.step_down(o.target_bps, 1);
    }
    let skipped = now_round - o.id - 1;
    if skipped > 0 {
        l.rounds_empty += skipped;
        l.streak = 0;
        next_bps = cfg.step_down(next_bps, skipped);
    }
    l.open = OpenRound {
        id: now_round,
        pot: 0,
        hunters: 0,
        win_delta: 0,
        lose_delta: 0,
        target_bps: next_bps,
    };
    (l, result)
}

// ---------- JSON views ----------

#[near(serializers = [json])]
pub struct RoundView {
    pub round: u64,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub pot: U128,
    pub hunters: u32,
    pub target_bps: u16,
    pub target: U128,
    pub supply_at_close: U128,
    pub won: bool,
    pub minted: U128,
    pub burned: U128,
    pub next_target_bps: u16,
}

#[near(serializers = [json])]
pub struct StatusView {
    pub now_ms: u64,
    pub round: u64,
    pub round_started_at_ms: u64,
    pub round_ends_at_ms: u64,
    pub pot: U128,
    pub hunters: u32,
    pub target_bps: u16,
    /// Pot needed if the round closed now (the supply can still change).
    pub target_now: U128,
    pub supply: U128,
    pub min_hunters: u32,
    /// "on_track" if the hunt would succeed if it closed now, else "short".
    pub outlook: String,
    pub streak: u32,
    pub best_streak: u32,
    pub rounds_won: u64,
    pub rounds_lost: u64,
    pub rounds_empty: u64,
    pub total_minted: U128,
    pub total_burned: U128,
    pub uncollected_mint: U128,
    pub uncollected_burn: U128,
    pub settled_rounds: u64,
    pub last_result: Option<RoundView>,
}

#[near(serializers = [json])]
pub struct HunterView {
    pub account_id: AccountId,
    pub registered: bool,
    pub faucet_claimed: bool,
    /// NEP-141 balance, including the result of a closed hunt not yet collected.
    pub balance: U128,
    /// "none", "hunting" (stake locked in the open round), "won" or "lost"
    /// (round closed, result already counted in `balance`).
    pub status: String,
    pub stake_round: Option<u64>,
    pub stake: U128,
    /// For "won"/"lost": what comes back. For "hunting": what would come back if the
    /// round closed now.
    pub returns: U128,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    token: FungibleToken,
    metadata: FungibleTokenMetadata,
    config: Config,
    genesis_ns: u64,
    ledger: Ledger,
    stakes: LookupMap<AccountId, Stake>,
    results: LookupMap<u64, RoundResult>,
    history: Vector<u64>,
    faucet_claimed: LookupSet<AccountId>,
}

#[near]
impl Contract {
    /// Deploy-time initializer. Only the contract account itself can call it, so it
    /// cannot be front-run between deploy and init.
    #[init]
    #[private]
    pub fn new(config: Config) -> Self {
        config.assert_valid();
        let metadata = FungibleTokenMetadata {
            spec: FT_METADATA_SPEC.to_string(),
            name: "Stag Hunt".to_string(),
            symbol: "STAG".to_string(),
            icon: Some(ICON.to_string()),
            reference: None,
            reference_hash: None,
            decimals: 18,
        };
        metadata.assert_valid();
        let mut token = FungibleToken::new(StorageKey::Accounts);
        // The contract account holds the pots, so it is registered from the start.
        token.internal_register_account(&env::current_account_id());
        Self {
            token,
            metadata,
            ledger: Ledger {
                open: OpenRound {
                    id: 0,
                    pot: 0,
                    hunters: 0,
                    win_delta: 0,
                    lose_delta: 0,
                    target_bps: config.start_target_bps,
                },
                pending_mint: 0,
                pending_burn: 0,
                escrow_settled: 0,
                streak: 0,
                best_streak: 0,
                rounds_won: 0,
                rounds_lost: 0,
                rounds_empty: 0,
                total_minted: 0,
                total_burned: 0,
            },
            config,
            genesis_ns: env::block_timestamp(),
            stakes: LookupMap::new(StorageKey::Stakes),
            results: LookupMap::new(StorageKey::Results),
            history: Vector::new(StorageKey::History),
            faucet_claimed: LookupSet::new(StorageKey::FaucetClaimed),
        }
    }

    // ---------- the game ----------

    /// Lock `amount` STAG into the current hunt. Needs exactly 1 yoctoNEAR attached
    /// (same protection as `ft_transfer`). A stake cannot be withdrawn before the
    /// round closes.
    #[payable]
    pub fn join_hunt(&mut self, amount: U128) -> HunterView {
        assert_one_yocto();
        let account = env::predecessor_account_id();
        self.settle();
        self.collect(&account);
        let amount: Balance = amount.0;
        require!(amount > 0, "amount must be positive");
        let round = self.ledger.open.id;
        let me = env::current_account_id();
        let memo = format!("hunt #{round}");
        self.token
            .internal_transfer(&account, &me, amount, Some(memo));

        let before = self.stakes.get(&account).map(|s| s.amount).unwrap_or(0);
        if before == 0 {
            self.ledger.open.hunters += 1;
        }
        let after = before + amount;
        let cfg = &self.config;
        self.ledger.open.win_delta += cfg.bonus(after) - cfg.bonus(before);
        self.ledger.open.lose_delta += cfg.burn(after) - cfg.burn(before);
        self.ledger.open.pot += amount;
        self.stakes.insert(
            account.clone(),
            Stake {
                round,
                amount: after,
            },
        );
        emit(
            "hunt_joined",
            json!({
                "account_id": account,
                "round": round,
                "amount": U128(amount),
                "stake": U128(after),
                "pot": U128(self.ledger.open.pot),
                "hunters": self.ledger.open.hunters,
            }),
        );
        self.get_hunter(account)
    }

    /// Collect the result of your last closed hunt into your stored balance. Any
    /// other call you make does this too, so you rarely need it.
    pub fn collect_hunt(&mut self) -> HunterView {
        let account = env::predecessor_account_id();
        self.settle();
        self.collect(&account);
        self.get_hunter(account)
    }

    /// Settle any closed round. Anyone may call it; every other method calls it first.
    pub fn settle_rounds(&mut self) -> StatusView {
        self.settle();
        self.get_status()
    }

    /// Mint the starter amount once per account while the faucet is open. If the
    /// caller is not registered, the attached deposit pays for registration
    /// (see `storage_balance_bounds`); anything above that is refunded.
    #[payable]
    pub fn claim_starter(&mut self) -> HunterView {
        let account = env::predecessor_account_id();
        require!(
            env::block_timestamp() / NS_PER_MS < self.config.faucet_closes_at_ms,
            "the faucet is closed"
        );
        require!(
            !self.faucet_claimed.contains(&account),
            "this account already claimed its starter STAG"
        );
        self.settle();
        let mut refund = env::attached_deposit();
        if !self.token.accounts.contains_key(&account) {
            let min = self.token.storage_balance_bounds().min;
            require!(
                refund >= min,
                format!(
                    "attach at least {} yoctoNEAR to register this account",
                    min.as_yoctonear()
                )
            );
            self.token.internal_register_account(&account);
            refund = refund.saturating_sub(min);
        }
        if refund > NearToken::from_yoctonear(0) {
            Promise::new(account.clone()).transfer(refund).detach();
        }
        self.faucet_claimed.insert(account.clone());
        let amount = self.config.faucet_amount.0;
        self.token.internal_deposit(&account, amount);
        FtMint {
            owner_id: &account,
            amount: U128(amount),
            memo: Some("starter"),
        }
        .emit();
        self.get_hunter(account)
    }

    // ---------- views ----------

    pub fn get_config(&self) -> Config {
        self.config.clone()
    }

    pub fn get_status(&self) -> StatusView {
        let (l, new_result) = self.virtual_ledger();
        let supply = self.token.total_supply + l.pending_mint - l.pending_burn;
        let target_now = target_amount(l.open.target_bps, supply);
        let on_track = l.open.pot > 0
            && l.open.pot >= target_now
            && l.open.hunters >= self.config.min_hunters;
        let settled_rounds_pre = self.history.len() as u64 + u64::from(new_result.is_some());
        let last_result = match new_result {
            Some(r) => Some(r),
            None => self
                .history
                .len()
                .checked_sub(1)
                .and_then(|i| self.history.get(i))
                .and_then(|id| self.results.get(id))
                .cloned(),
        };
        let settled_rounds = settled_rounds_pre;
        StatusView {
            now_ms: env::block_timestamp() / NS_PER_MS,
            round: l.open.id,
            round_started_at_ms: self.round_start_ms(l.open.id),
            round_ends_at_ms: self.round_start_ms(l.open.id + 1),
            pot: U128(l.open.pot),
            hunters: l.open.hunters,
            target_bps: l.open.target_bps,
            target_now: U128(target_now),
            supply: U128(supply),
            min_hunters: self.config.min_hunters,
            outlook: if on_track { "on_track" } else { "short" }.to_string(),
            streak: l.streak,
            best_streak: l.best_streak,
            rounds_won: l.rounds_won,
            rounds_lost: l.rounds_lost,
            rounds_empty: l.rounds_empty,
            total_minted: U128(l.total_minted),
            total_burned: U128(l.total_burned),
            uncollected_mint: U128(l.pending_mint),
            uncollected_burn: U128(l.pending_burn),
            settled_rounds,
            last_result: last_result.map(|r| self.round_view(&r)),
        }
    }

    /// Closed rounds that had at least one hunter, newest first.
    pub fn get_rounds(&self, from_index: Option<u64>, limit: Option<u64>) -> Vec<RoundView> {
        let (_, new_result) = self.virtual_ledger();
        let mut all: Vec<RoundView> = Vec::new();
        let skip = from_index.unwrap_or(0);
        let limit = limit.unwrap_or(20).min(MAX_PAGE);
        let mut idx: u64 = 0;
        if let Some(r) = new_result {
            if idx >= skip {
                all.push(self.round_view(&r));
            }
            idx += 1;
        }
        let len = self.history.len() as u64;
        let mut i = len;
        while i > 0 && (all.len() as u64) < limit {
            i -= 1;
            if idx >= skip {
                let id = self.history.get(i as u32).unwrap();
                all.push(self.round_view(self.results.get(id).unwrap()));
            }
            idx += 1;
        }
        all
    }

    pub fn get_hunter(&self, account_id: AccountId) -> HunterView {
        let (l, new_result) = self.virtual_ledger();
        let registered = self.token.accounts.contains_key(&account_id);
        let stored = self.token.accounts.get(&account_id).unwrap_or(0);
        let mut view = HunterView {
            account_id: account_id.clone(),
            registered,
            faucet_claimed: self.faucet_claimed.contains(&account_id),
            balance: U128(stored),
            status: "none".to_string(),
            stake_round: None,
            stake: U128(0),
            returns: U128(0),
        };
        if account_id == env::current_account_id() {
            view.balance = U128(stored - l.escrow_settled);
            return view;
        }
        if let Some(stake) = self.stakes.get(&account_id) {
            view.stake_round = Some(stake.round);
            view.stake = U128(stake.amount);
            if stake.round < l.open.id {
                let result = self.result_for(stake.round, &new_result);
                let back = self.returns(stake.amount, result.won);
                view.status = if result.won { "won" } else { "lost" }.to_string();
                view.returns = U128(back);
                view.balance = U128(stored + back);
            } else {
                view.status = "hunting".to_string();
                let supply = self.token.total_supply + l.pending_mint - l.pending_burn;
                let won_now = l.open.pot >= target_amount(l.open.target_bps, supply)
                    && l.open.hunters >= self.config.min_hunters;
                view.returns = U128(self.returns(stake.amount, won_now));
            }
        }
        view
    }

    /// Unix ms at which round `round` starts.
    pub fn round_start_ms(&self, round: u64) -> u64 {
        (self.genesis_ns + round * self.config.round_secs * NS_PER_SEC) / NS_PER_MS
    }
}

fn emit(event: &str, data: near_sdk::serde_json::Value) {
    env::log_str(&format!(
        "EVENT_JSON:{}",
        json!({"standard": "staghunt", "version": "1.0.0", "event": event, "data": [data]})
    ));
}

// ---------- internals ----------
impl Contract {
    fn now_round(&self) -> u64 {
        (env::block_timestamp() - self.genesis_ns) / (self.config.round_secs * NS_PER_SEC)
    }

    fn virtual_ledger(&self) -> (Ledger, Option<RoundResult>) {
        settle_ledger(
            &self.config,
            &self.ledger,
            self.token.total_supply,
            self.now_round(),
        )
    }

    fn settle(&mut self) {
        let (l, result) = self.virtual_ledger();
        if let Some(r) = result {
            emit(
                "hunt_settled",
                json!({
                    "round": r.round,
                    "won": r.won,
                    "pot": U128(r.pot),
                    "target": U128(r.target),
                    "hunters": r.hunters,
                    "supply_at_close": U128(r.supply_at_close),
                    "minted": U128(if r.won { r.delta } else { 0 }),
                    "burned": U128(if r.won { 0 } else { r.delta }),
                    "next_target_bps": r.next_target_bps,
                }),
            );
            self.history.push(r.round);
            self.results.insert(r.round, r);
        }
        self.ledger = l;
    }

    fn result_for<'a>(
        &'a self,
        round: u64,
        new_result: &'a Option<RoundResult>,
    ) -> &'a RoundResult {
        match new_result {
            Some(r) if r.round == round => r,
            _ => self
                .results
                .get(&round)
                .unwrap_or_else(|| env::panic_str("missing round result")),
        }
    }

    fn returns(&self, stake: Balance, won: bool) -> Balance {
        if won {
            stake + self.config.bonus(stake)
        } else {
            stake - self.config.burn(stake)
        }
    }

    /// Move the result of `account`'s closed hunt into its stored balance.
    /// Must run after `settle`.
    fn collect(&mut self, account: &AccountId) {
        let Some(stake) = self.stakes.get(account).cloned() else {
            return;
        };
        if stake.round >= self.ledger.open.id {
            return;
        }
        let won = self
            .results
            .get(&stake.round)
            .unwrap_or_else(|| env::panic_str("missing round result"))
            .won;
        let me = env::current_account_id();
        self.token.internal_withdraw(&me, stake.amount);
        self.ledger.escrow_settled -= stake.amount;
        if won {
            let bonus = self.config.bonus(stake.amount);
            self.token.internal_deposit(account, stake.amount + bonus);
            self.ledger.pending_mint -= bonus;
            FtTransfer {
                old_owner_id: &me,
                new_owner_id: account,
                amount: U128(stake.amount),
                memo: Some(format!("hunt #{} stake back", stake.round).as_str()),
            }
            .emit();
            if bonus > 0 {
                FtMint {
                    owner_id: account,
                    amount: U128(bonus),
                    memo: Some(format!("hunt #{} won", stake.round).as_str()),
                }
                .emit();
            }
        } else {
            let burn = self.config.burn(stake.amount);
            let back = stake.amount - burn;
            self.token.internal_deposit(account, back);
            self.ledger.pending_burn -= burn;
            if back > 0 {
                FtTransfer {
                    old_owner_id: &me,
                    new_owner_id: account,
                    amount: U128(back),
                    memo: Some(format!("hunt #{} stake back", stake.round).as_str()),
                }
                .emit();
            }
            if burn > 0 {
                FtBurn {
                    owner_id: &me,
                    amount: U128(burn),
                    memo: Some(format!("hunt #{} lost", stake.round).as_str()),
                }
                .emit();
            }
        }
        self.stakes.remove(account);
    }

    fn round_view(&self, r: &RoundResult) -> RoundView {
        RoundView {
            round: r.round,
            started_at_ms: self.round_start_ms(r.round),
            ended_at_ms: self.round_start_ms(r.round + 1),
            pot: U128(r.pot),
            hunters: r.hunters,
            target_bps: r.target_bps,
            target: U128(r.target),
            supply_at_close: U128(r.supply_at_close),
            won: r.won,
            minted: U128(if r.won { r.delta } else { 0 }),
            burned: U128(if r.won { 0 } else { r.delta }),
            next_target_bps: r.next_target_bps,
        }
    }
}

// ---------- NEP-141 ----------

#[near]
impl FungibleTokenCore for Contract {
    #[payable]
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        let sender = env::predecessor_account_id();
        require!(
            receiver_id != env::current_account_id(),
            "use join_hunt to send STAG to the hunt"
        );
        self.settle();
        self.collect(&sender);
        self.token.ft_transfer(receiver_id, amount, memo)
    }

    #[payable]
    fn ft_transfer_call(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>,
        msg: String,
    ) -> PromiseOrValue<U128> {
        let sender = env::predecessor_account_id();
        require!(
            receiver_id != env::current_account_id(),
            "use join_hunt to send STAG to the hunt"
        );
        self.settle();
        self.collect(&sender);
        self.token.ft_transfer_call(receiver_id, amount, memo, msg)
    }

    /// Supply including the result of every closed hunt, collected or not.
    fn ft_total_supply(&self) -> U128 {
        let (l, _) = self.virtual_ledger();
        U128(self.token.total_supply + l.pending_mint - l.pending_burn)
    }

    /// Balance including the result of a closed hunt not yet collected. For the
    /// contract account itself: the pot of the open hunt.
    fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        self.get_hunter(account_id).balance
    }
}

#[near]
impl FungibleTokenResolver for Contract {
    #[private]
    fn ft_resolve_transfer(
        &mut self,
        sender_id: AccountId,
        receiver_id: AccountId,
        amount: U128,
    ) -> U128 {
        let (used, burned) =
            self.token
                .internal_ft_resolve_transfer(&sender_id, receiver_id, amount);
        if burned > 0 {
            log!("{} STAG burned because the sender account was closed", burned);
        }
        used.into()
    }
}

#[near]
impl StorageManagement for Contract {
    #[payable]
    fn storage_deposit(
        &mut self,
        account_id: Option<AccountId>,
        registration_only: Option<bool>,
    ) -> StorageBalance {
        self.token.storage_deposit(account_id, registration_only)
    }

    #[payable]
    fn storage_withdraw(&mut self, amount: Option<NearToken>) -> StorageBalance {
        self.token.storage_withdraw(amount)
    }

    /// Unregistering first collects any closed hunt. It fails while you have STAG
    /// locked in the open hunt.
    #[payable]
    fn storage_unregister(&mut self, force: Option<bool>) -> bool {
        assert_one_yocto();
        let account = env::predecessor_account_id();
        require!(
            account != env::current_account_id(),
            "the contract account cannot unregister"
        );
        self.settle();
        self.collect(&account);
        require!(
            !self.stakes.contains_key(&account),
            "you have STAG locked in the open hunt"
        );
        if let Some((id, balance)) = self.token.internal_storage_unregister(force) {
            if balance > 0 {
                FtBurn {
                    owner_id: &id,
                    amount: U128(balance),
                    memo: Some("account closed"),
                }
                .emit();
            }
            true
        } else {
            false
        }
    }

    fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        self.token.storage_balance_bounds()
    }

    fn storage_balance_of(&self, account_id: AccountId) -> Option<StorageBalance> {
        self.token.storage_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenMetadataProvider for Contract {
    fn ft_metadata(&self) -> FungibleTokenMetadata {
        self.metadata.clone()
    }
}

#[cfg(test)]
mod tests;
