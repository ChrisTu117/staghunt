//! Unit tests for the hunt rules. Each test names the README claim it backs.

use super::*;
use near_sdk::test_utils::{VMContextBuilder, get_logs};
use near_sdk::testing_env;

const T0: u64 = 1_800_000_000_000_000_000; // ns
const STAG: u128 = 1_000_000_000_000_000_000;
const ROUND: u64 = 60;

fn contract_id() -> AccountId {
    "stag.near".parse().unwrap()
}

fn acc(name: &str) -> AccountId {
    format!("{name}.near").parse().unwrap()
}

fn cfg() -> Config {
    Config {
        round_secs: ROUND,
        reward_bps: 1000,
        burn_bps: 500,
        start_target_bps: 2000,
        target_step_up_bps: 500,
        target_step_down_bps: 500,
        min_target_bps: 500,
        max_target_bps: 6000,
        min_hunters: 3,
        faucet_amount: U128(100 * STAG),
        faucet_closes_at_ms: (T0 / NS_PER_MS) + 365 * 24 * 3600 * 1000,
    }
}

struct World {
    c: Contract,
    now: u64,
}

impl World {
    fn new(config: Config) -> Self {
        // start from empty storage, even when one test builds several worlds
        let ctx = VMContextBuilder::new()
            .current_account_id(contract_id())
            .predecessor_account_id(contract_id())
            .block_timestamp(T0)
            .build();
        near_sdk::env::set_blockchain_interface(near_sdk::MockedBlockchain::new(
            ctx,
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            vec![],
            Default::default(),
            Default::default(),
            None,
        ));
        let c = Contract::new(config);
        World { c, now: T0 }
    }

    fn as_(&self, who: &AccountId, deposit: u128) {
        set_ctx(who, deposit, self.now);
    }

    fn advance_secs(&mut self, secs: u64) {
        self.now += secs * NS_PER_SEC;
    }

    fn faucet(&mut self, who: &AccountId) -> HunterView {
        self.as_(who, NearToken::from_millinear(10).as_yoctonear());
        self.c.claim_starter()
    }

    fn join(&mut self, who: &AccountId, amount: u128) -> HunterView {
        self.as_(who, 1);
        self.c.join_hunt(U128(amount))
    }

    fn transfer(&mut self, from: &AccountId, to: &AccountId, amount: u128) {
        self.as_(from, 1);
        self.c.ft_transfer(to.clone(), U128(amount), None);
    }

    fn view(&self) {
        set_ctx(&acc("viewer"), 0, self.now);
    }

    fn bal(&self, who: &AccountId) -> u128 {
        self.view();
        self.c.ft_balance_of(who.clone()).0
    }

    fn supply(&self) -> u128 {
        self.view();
        self.c.ft_total_supply().0
    }

    fn status(&self) -> StatusView {
        self.view();
        self.c.get_status()
    }

    /// Sum of NEP-141 balances of `accounts` plus the contract account.
    fn sum_balances(&self, accounts: &[AccountId]) -> u128 {
        accounts.iter().map(|a| self.bal(a)).sum::<u128>() + self.bal(&contract_id())
    }
}

fn set_ctx(predecessor: &AccountId, deposit: u128, now: u64) {
    let ctx = VMContextBuilder::new()
        .current_account_id(contract_id())
        .predecessor_account_id(predecessor.clone())
        .signer_account_id(predecessor.clone())
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .block_timestamp(now)
        .build();
    testing_env!(ctx);
}

fn three_hunters(w: &mut World) -> [AccountId; 3] {
    let (a, b, c) = (acc("alice"), acc("bob"), acc("carol"));
    w.faucet(&a);
    w.faucet(&b);
    w.faucet(&c);
    [a, b, c]
}

// Claim: "the faucet mints 100 STAG once per account"
#[test]
fn faucet_mints_once() {
    let mut w = World::new(cfg());
    let a = acc("alice");
    let v = w.faucet(&a);
    assert_eq!(v.balance.0, 100 * STAG);
    assert!(v.faucet_claimed);
    assert_eq!(w.supply(), 100 * STAG);
}

#[test]
#[should_panic(expected = "already claimed")]
fn faucet_second_claim_fails() {
    let mut w = World::new(cfg());
    let a = acc("alice");
    w.faucet(&a);
    w.faucet(&a);
}

#[test]
#[should_panic(expected = "to register this account")]
fn faucet_needs_storage_deposit_when_unregistered() {
    let mut w = World::new(cfg());
    w.as_(&acc("alice"), 0);
    w.c.claim_starter();
}

#[test]
#[should_panic(expected = "the faucet is closed")]
fn faucet_closes() {
    let mut c = cfg();
    c.faucet_closes_at_ms = T0 / NS_PER_MS + 1000;
    let mut w = World::new(c);
    w.advance_secs(2);
    w.faucet(&acc("alice"));
}

// Claim: "if the pot reaches the target and 3+ accounts hunt, each hunter gets stake + 10%"
#[test]
fn successful_hunt_mints_ten_percent() {
    let mut w = World::new(cfg());
    let [a, b, c] = three_hunters(&mut w);
    // supply 300, target 20% = 60
    w.join(&a, 30 * STAG);
    w.join(&b, 20 * STAG);
    w.join(&c, 10 * STAG);
    let s = w.status();
    assert_eq!(s.target_now.0, 60 * STAG);
    assert_eq!(s.pot.0, 60 * STAG);
    assert_eq!(s.outlook, "on_track");
    w.advance_secs(ROUND);
    assert_eq!(w.bal(&a), 70 * STAG + 33 * STAG);
    assert_eq!(w.bal(&b), 80 * STAG + 22 * STAG);
    assert_eq!(w.bal(&c), 90 * STAG + 11 * STAG);
    assert_eq!(w.supply(), 306 * STAG);
    let s = w.status();
    assert_eq!(s.round, 1);
    assert_eq!(s.target_bps, 2500, "target rises 5 points after a win");
    assert_eq!(s.streak, 1);
    assert_eq!(s.rounds_won, 1);
    let last = s.last_result.unwrap();
    assert!(last.won);
    assert_eq!(last.minted.0, 6 * STAG);
    assert_eq!(w.sum_balances(&[a, b, c]), w.supply());
}

// Claim: "if the pot is short, each hunter loses 5% of the stake (burned)"
#[test]
fn short_hunt_burns_five_percent() {
    let mut w = World::new(cfg());
    let [a, b, c] = three_hunters(&mut w);
    w.join(&a, 30 * STAG);
    w.join(&b, 20 * STAG);
    w.join(&c, 9 * STAG); // pot 59 < 60
    assert_eq!(w.status().outlook, "short");
    w.advance_secs(ROUND);
    assert_eq!(w.bal(&a), 70 * STAG + 30 * STAG * 95 / 100);
    assert_eq!(w.bal(&b), 80 * STAG + 20 * STAG * 95 / 100);
    assert_eq!(w.bal(&c), 91 * STAG + 9 * STAG * 95 / 100);
    let burned = (30 + 20 + 9) * STAG * 5 / 100;
    assert_eq!(w.supply(), 300 * STAG - burned);
    let s = w.status();
    assert_eq!(s.target_bps, 1500, "target falls 5 points after a loss");
    assert_eq!(s.streak, 0);
    assert_eq!(s.rounds_lost, 1);
    assert_eq!(s.total_burned.0, burned);
    assert_eq!(w.sum_balances(&[a, b, c]), w.supply());
}

// Claim: "a hunt with fewer than 3 hunters fails even if the pot is big enough"
#[test]
fn whale_alone_fails() {
    let mut w = World::new(cfg());
    let [a, _b, _c] = three_hunters(&mut w);
    w.join(&a, 100 * STAG); // 33% of supply, but 1 hunter
    w.advance_secs(ROUND);
    assert_eq!(w.bal(&a), 95 * STAG);
    assert!(!w.status().last_result.unwrap().won);
}

// Claim: "rounds nobody joins lower the target 5 points each, down to 5%"
#[test]
fn empty_rounds_lower_target_to_floor() {
    let mut w = World::new(cfg());
    three_hunters(&mut w);
    w.advance_secs(ROUND * 2);
    let s = w.status();
    assert_eq!(s.round, 2);
    assert_eq!(s.target_bps, 1000);
    assert_eq!(s.rounds_empty, 2);
    w.advance_secs(ROUND * 10);
    assert_eq!(w.status().target_bps, 500);
    // settling for real gives the same numbers
    w.as_(&acc("bob"), 0);
    let s = w.c.settle_rounds();
    assert_eq!(s.target_bps, 500);
    assert_eq!(s.rounds_empty, 12);
}

// Claim: "the target never goes above 60%"
#[test]
fn target_capped_at_max() {
    let mut c = cfg();
    c.start_target_bps = 5800;
    let mut w = World::new(c);
    let [a, b, cc] = three_hunters(&mut w);
    for _ in 0..2 {
        let (ba, bb, bc) = (w.bal(&a), w.bal(&b), w.bal(&cc));
        w.join(&a, ba);
        w.join(&b, bb);
        w.join(&cc, bc);
        w.advance_secs(ROUND);
        assert!(w.status().last_result.unwrap().won);
        assert_eq!(w.status().target_bps, 6000);
    }
    assert_eq!(w.status().streak, 2);
    assert_eq!(w.status().best_streak, 2);
}

// Claim: "a stake stays locked until the round closes"
#[test]
#[should_panic(expected = "doesn't have enough balance")]
fn stake_is_locked() {
    let mut w = World::new(cfg());
    let [a, b, _] = three_hunters(&mut w);
    w.join(&a, 80 * STAG);
    w.transfer(&a, &b, 21 * STAG);
}

#[test]
#[should_panic(expected = "use join_hunt")]
fn cannot_transfer_to_contract() {
    let mut w = World::new(cfg());
    let [a, _, _] = three_hunters(&mut w);
    w.transfer(&a, &contract_id(), STAG);
}

#[test]
#[should_panic(expected = "Requires attached deposit of exactly 1 yoctoNEAR")]
fn join_needs_one_yocto() {
    let mut w = World::new(cfg());
    let [a, _, _] = three_hunters(&mut w);
    w.as_(&a, 0);
    w.c.join_hunt(U128(STAG));
}

// Claim: "joining twice in a round counts one hunter; rounding is per account and exact"
#[test]
fn per_account_rounding_is_exact() {
    let mut c = cfg();
    c.faucet_amount = U128(1000); // tiny base units to expose rounding
    c.min_target_bps = 100;
    c.start_target_bps = 100; // target = 1% of 3000 = 30 <= pot 47
    let mut w = World::new(c);
    let [a, b, cc] = three_hunters(&mut w);
    w.join(&a, 7);
    w.join(&a, 6); // a: 13 -> bonus 1 (not 0 + 0)
    w.join(&b, 9); // bonus 0
    w.join(&cc, 25); // bonus 2
    let s = w.status();
    assert_eq!(s.hunters, 3);
    assert_eq!(s.pot.0, 47);
    w.advance_secs(ROUND);
    assert_eq!(w.supply(), 3000 + 1 + 0 + 2);
    assert_eq!(w.bal(&a), 1000 + 1);
    assert_eq!(w.bal(&b), 1000);
    assert_eq!(w.bal(&cc), 1000 + 2);
    // collecting for real keeps the numbers
    w.transfer(&a, &b, 1);
    w.transfer(&b, &a, 1);
    w.transfer(&cc, &a, 1);
    assert_eq!(w.bal(&a), 1001 + 1 - 1 + 1 + 1 - 1);
    assert_eq!(w.supply(), 3003);
    assert_eq!(w.c.token.total_supply, 3003);
    assert_eq!(w.c.ledger.pending_mint, 0);
    assert_eq!(w.c.ledger.escrow_settled, 0);
}

// Claim: "only what happened inside the round counts": a faucet claim after the close
// cannot raise the closed round's target.
#[test]
fn late_mint_does_not_change_closed_round() {
    let mut w = World::new(cfg());
    let [a, b, c] = three_hunters(&mut w);
    w.join(&a, 30 * STAG);
    w.join(&b, 20 * STAG);
    w.join(&c, 10 * STAG); // exactly 60 = 20% of 300
    w.advance_secs(ROUND + 5);
    w.faucet(&acc("dave")); // supply would be 400 -> 80 needed, but the round is closed
    let s = w.status();
    let last = s.last_result.unwrap();
    assert!(last.won);
    assert_eq!(last.supply_at_close.0, 300 * STAG);
    assert_eq!(w.supply(), 406 * STAG);
}

// Claim: "balances and supply are right the moment a round closes, before anyone
// sends a transaction" (view == settled state)
#[test]
fn views_match_settled_state() {
    let mut w = World::new(cfg());
    let [a, b, c] = three_hunters(&mut w);
    w.join(&a, 40 * STAG);
    w.join(&b, 20 * STAG);
    w.join(&c, 1 * STAG);
    w.advance_secs(ROUND * 3 + 1);
    let before: Vec<u128> = [&a, &b, &c, &contract_id()].iter().map(|x| w.bal(x)).collect();
    let supply_before = w.supply();
    w.view();
    let st_before = near_sdk::serde_json::to_string(&w.c.get_status()).unwrap();
    let rounds_before = near_sdk::serde_json::to_string(&w.c.get_rounds(None, None)).unwrap();

    w.as_(&acc("keeper"), 0);
    w.c.settle_rounds();
    let logs = get_logs();
    assert!(logs.iter().any(|l| l.contains("\"event\":\"hunt_settled\"")));

    let after: Vec<u128> = [&a, &b, &c, &contract_id()].iter().map(|x| w.bal(x)).collect();
    assert_eq!(before, after);
    assert_eq!(supply_before, w.supply());
    w.view();
    assert_eq!(st_before, near_sdk::serde_json::to_string(&w.c.get_status()).unwrap());
    assert_eq!(rounds_before, near_sdk::serde_json::to_string(&w.c.get_rounds(None, None)).unwrap());
}

#[test]
#[should_panic(expected = "need min_target_bps <= start_target_bps <= max_target_bps")]
fn init_rejects_bad_config() {
    let mut c = cfg();
    c.start_target_bps = 100;
    set_ctx(&contract_id(), 0, T0);
    Contract::new(c);
}

#[test]
#[should_panic(expected = "locked in the open hunt")]
fn cannot_unregister_with_open_stake() {
    let mut w = World::new(cfg());
    let [a, _, _] = three_hunters(&mut w);
    w.join(&a, STAG);
    w.as_(&a, 1);
    w.c.storage_unregister(Some(true));
}

// Claim: "every closed round with hunters is listed, newest first"
#[test]
fn round_history_newest_first() {
    let mut w = World::new(cfg());
    let [a, b, c] = three_hunters(&mut w);
    for i in 0..3u128 {
        w.join(&a, (10 + i) * STAG);
        w.join(&b, 30 * STAG);
        w.join(&c, 30 * STAG);
        w.advance_secs(ROUND * 2); // one round with hunters, one empty
    }
    w.view();
    let rounds = w.c.get_rounds(None, None);
    let ids: Vec<u64> = rounds.iter().map(|r| r.round).collect();
    assert_eq!(ids, vec![4, 2, 0]);
    let page = w.c.get_rounds(Some(1), Some(1));
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].round, 2);
}

/// Tiny deterministic PRNG so the fuzz test needs no extra crate.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

// Claim: "sum of all balances always equals total supply" (NEP-141 consistency),
// under random joins, transfers, faucet claims and time jumps.
#[test]
fn supply_invariant_under_random_play() {
    for seed in 1..=4u64 {
        let mut c = cfg();
        c.min_hunters = 2;
        c.round_secs = 30;
        let mut w = World::new(c);
        let names: Vec<AccountId> = (0..6).map(|i| acc(&format!("p{i}"))).collect();
        let mut rng = Lcg(seed);
        for step in 0..200 {
            let who = &names[rng.below(names.len() as u64) as usize];
            match rng.below(6) {
                0 => {
                    w.view();
                    if !w.c.get_hunter(who.clone()).faucet_claimed {
                        w.faucet(who);
                    }
                }
                1 | 2 => {
                    w.view();
                    let h = w.c.get_hunter(who.clone());
                    if h.registered && h.balance.0 > 0 {
                        let amt = 1 + (rng.next() as u128 * 7919) % h.balance.0;
                        w.join(who, amt);
                    }
                }
                3 => {
                    let to = &names[rng.below(names.len() as u64) as usize];
                    w.view();
                    let h = w.c.get_hunter(who.clone());
                    let reg_to = w.c.get_hunter(to.clone()).registered;
                    if to != who && reg_to && h.balance.0 > 0 {
                        let amt = 1 + (rng.next() as u128 * 104729) % h.balance.0;
                        w.transfer(who, to, amt);
                    }
                }
                4 => w.advance_secs(rng.below(70)),
                _ => {
                    w.as_(who, 0);
                    w.c.collect_hunt();
                }
            }
            assert_eq!(
                w.sum_balances(&names),
                w.supply(),
                "seed {seed} step {step}: balances must add up to supply"
            );
            let stored: u128 = names
                .iter()
                .chain(std::iter::once(&contract_id()))
                .map(|a| w.c.token.accounts.get(a).unwrap_or(0))
                .sum();
            assert_eq!(stored, w.c.token.total_supply, "seed {seed} step {step}: stored ledger");
        }
    }
}

// Claim: "an independent Python model (sim/staghunt_sim.py) and the contract agree to
// the last base unit on 300+ random operations" — the reference data does not come
// from the contract's own code.
#[test]
fn crosscheck_with_python_model() {
    use near_sdk::serde_json::Value;
    let data: Value =
        near_sdk::serde_json::from_str(include_str!("../sim/crosscheck.json")).unwrap();
    let c = &data["config"];
    let u = |v: &Value| v.as_u64().unwrap();
    let big = |v: &Value| v.as_str().unwrap().parse::<u128>().unwrap();
    let config = Config {
        round_secs: u(&c["round_secs"]),
        reward_bps: u(&c["reward_bps"]) as u16,
        burn_bps: u(&c["burn_bps"]) as u16,
        start_target_bps: u(&c["start_target_bps"]) as u16,
        target_step_up_bps: u(&c["target_step_up_bps"]) as u16,
        target_step_down_bps: u(&c["target_step_down_bps"]) as u16,
        min_target_bps: u(&c["min_target_bps"]) as u16,
        max_target_bps: u(&c["max_target_bps"]) as u16,
        min_hunters: u(&c["min_hunters"]) as u32,
        faucet_amount: U128(big(&c["faucet_amount"])),
        faucet_closes_at_ms: T0 / NS_PER_MS + u(&c["faucet_closes_at_s"]) * 1000,
    };
    let mut w = World::new(config);
    let name = |v: &Value| acc(v.as_str().unwrap());
    let accounts: Vec<AccountId> = data["accounts"].as_array().unwrap().iter().map(name).collect();
    let ops = data["ops"].as_array().unwrap();
    assert!(ops.len() > 300);
    for (i, op) in ops.iter().enumerate() {
        match op["op"].as_str().unwrap() {
            "faucet" => {
                w.faucet(&name(&op["who"]));
            }
            "join" => {
                w.join(&name(&op["who"]), big(&op["amount"]));
            }
            "transfer" => w.transfer(&name(&op["from"]), &name(&op["to"]), big(&op["amount"])),
            "advance" => w.advance_secs(u(&op["secs"])),
            other => panic!("unknown op {other}"),
        }
        let e = &op["expect"];
        for a in &accounts {
            let key = a.as_str().trim_end_matches(".near");
            assert_eq!(w.bal(a), big(&e["balances"][key]), "op {i}: balance of {key}");
        }
        let s = w.status();
        assert_eq!(s.pot.0, big(&e["pot"]), "op {i}: pot");
        assert_eq!(s.supply.0, big(&e["supply"]), "op {i}: supply");
        assert_eq!(w.supply(), big(&e["supply"]), "op {i}: ft_total_supply");
        assert_eq!(s.round, u(&e["round"]), "op {i}: round");
        assert_eq!(s.target_bps as u64, u(&e["target_bps"]), "op {i}: target_bps");
        assert_eq!(s.streak as u64, u(&e["streak"]), "op {i}: streak");
        assert_eq!(s.best_streak as u64, u(&e["best_streak"]), "op {i}: best_streak");
        assert_eq!(s.rounds_won, u(&e["rounds_won"]), "op {i}: rounds_won");
        assert_eq!(s.rounds_lost, u(&e["rounds_lost"]), "op {i}: rounds_lost");
        assert_eq!(s.rounds_empty, u(&e["rounds_empty"]), "op {i}: rounds_empty");
        assert_eq!(s.total_minted.0, big(&e["total_minted"]), "op {i}: total_minted");
        assert_eq!(s.total_burned.0, big(&e["total_burned"]), "op {i}: total_burned");
    }
}
