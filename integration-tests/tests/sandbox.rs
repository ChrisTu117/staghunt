//! Runs the real wasm (res/staghunt.wasm, built by `scripts/build.sh`) in a local
//! NEAR sandbox node: real receipts, real gas, real block time.

use near_api::{AccountId, NearGas, NearToken, NetworkConfig, Signer};
use serde_json::{Value, json};
use std::sync::Arc;

const STAG: u128 = 1_000_000_000_000_000_000;

struct Env {
    sandbox: near_sandbox::Sandbox,
    net: NetworkConfig,
    signer: Arc<Signer>,
    contract: AccountId,
}

async fn account(sandbox: &near_sandbox::Sandbox, name: &str) -> testresult::TestResult<AccountId> {
    let id: AccountId = name.parse()?;
    sandbox
        .create_account(id.as_str().parse()?)
        .initial_balance(NearToken::from_near(20))
        .send()
        .await?;
    Ok(id)
}

fn wasm() -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../res/staghunt.wasm");
    std::fs::read(path).expect("res/staghunt.wasm missing: run scripts/build.sh first")
}

fn config(round_secs: u64) -> Value {
    json!({
        "round_secs": round_secs,
        "reward_bps": 1000,
        "burn_bps": 500,
        "start_target_bps": 2000,
        "target_step_up_bps": 500,
        "target_step_down_bps": 500,
        "min_target_bps": 500,
        "max_target_bps": 6000,
        "min_hunters": 3,
        "faucet_amount": (100 * STAG).to_string(),
        "faucet_closes_at_ms": 4_000_000_000_000u64,
    })
}

async fn setup(round_secs: u64) -> testresult::TestResult<Env> {
    let sandbox = near_sandbox::Sandbox::start_sandbox().await?;
    let net = NetworkConfig::from_rpc_url("sandbox", sandbox.rpc_addr.parse()?);
    let signer = Signer::from_secret_key(
        near_sandbox::config::DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY.parse()?,
    )?;
    let contract = account(&sandbox, "stag.sandbox").await?;
    near_api::Contract::deploy(contract.clone())
        .use_code(wasm())
        .with_init_call("new", json!({ "config": config(round_secs) }))?
        .with_signer(signer.clone())
        .send_to(&net)
        .await?
        .assert_success();
    Ok(Env { sandbox, net, signer, contract })
}

impl Env {
    async fn call(&self, who: &AccountId, method: &str, args: Value, deposit: u128) -> testresult::TestResult<bool> {
        let r = near_api::Contract(self.contract.clone())
            .call_function(method, args)
            .transaction()
            .deposit(NearToken::from_yoctonear(deposit))
            .gas(NearGas::from_tgas(100))
            .with_signer(who.clone(), self.signer.clone())
            .send_to(&self.net)
            .await?;
        Ok(r.into_result().is_ok())
    }

    async fn view(&self, method: &str, args: Value) -> testresult::TestResult<Value> {
        Ok(near_api::Contract(self.contract.clone())
            .call_function(method, args)
            .read_only::<Value>()
            .fetch_from(&self.net)
            .await?
            .data)
    }

    async fn bal(&self, who: &AccountId) -> testresult::TestResult<u128> {
        let v = self.view("ft_balance_of", json!({ "account_id": who })).await?;
        Ok(v.as_str().unwrap().parse()?)
    }

    async fn supply(&self) -> testresult::TestResult<u128> {
        Ok(self.view("ft_total_supply", json!({})).await?.as_str().unwrap().parse()?)
    }

    async fn round(&self) -> testresult::TestResult<u64> {
        Ok(self.view("get_status", json!({})).await?["round"].as_u64().unwrap())
    }

    /// Produce blocks until the hunt in progress has closed.
    async fn close_round(&self) -> testresult::TestResult<()> {
        let start = self.round().await?;
        for _ in 0..60 {
            self.sandbox.fast_forward(20).await?;
            if self.round().await? > start {
                return Ok(());
            }
        }
        panic!("round did not close");
    }
}

#[tokio::test]
async fn hunts_on_a_real_node() -> testresult::TestResult<()> {
    let env = setup(30).await?;
    let alice = account(&env.sandbox, "alice.sandbox").await?;
    let bob = account(&env.sandbox, "bob.sandbox").await?;
    let carol = account(&env.sandbox, "carol.sandbox").await?;
    for a in [&alice, &bob, &carol] {
        assert!(env.call(a, "claim_starter", json!({}), 10u128.pow(22)).await?);
    }
    assert_eq!(env.supply().await?, 300 * STAG);
    // second claim fails
    assert!(!env.call(&alice, "claim_starter", json!({}), 0).await?);

    // win: 60 = 20% of 300, 3 hunters
    env.close_round().await?; // start at the beginning of a fresh hunt
    for (a, amt) in [(&alice, 30), (&bob, 20), (&carol, 10)] {
        assert!(env.call(a, "join_hunt", json!({ "amount": (amt * STAG).to_string() }), 1).await?);
    }
    // a stake is locked: alice has 70 free, cannot send 71
    assert!(!env.call(&alice, "ft_transfer", json!({ "receiver_id": bob, "amount": (71 * STAG).to_string() }), 1).await?);
    // STAG cannot be sent to the contract outside join_hunt
    assert!(!env.call(&alice, "ft_transfer", json!({ "receiver_id": env.contract, "amount": STAG.to_string() }), 1).await?);
    env.close_round().await?;
    assert_eq!(env.bal(&alice).await?, 103 * STAG);
    assert_eq!(env.bal(&bob).await?, 102 * STAG);
    assert_eq!(env.bal(&carol).await?, 101 * STAG);
    assert_eq!(env.supply().await?, 306 * STAG);
    assert_eq!(env.bal(&env.contract).await?, 0, "settled stakes belong to the hunters");

    // the won amount is spendable right away (ft_transfer collects first)
    assert!(env.call(&alice, "ft_transfer", json!({ "receiver_id": bob, "amount": (103 * STAG).to_string() }), 1).await?);
    assert_eq!(env.bal(&alice).await?, 0);
    assert_eq!(env.bal(&bob).await?, 205 * STAG);

    // lose: bob alone (1 hunter < 3)
    assert!(env.call(&bob, "join_hunt", json!({ "amount": (100 * STAG).to_string() }), 1).await?);
    env.close_round().await?;
    assert_eq!(env.bal(&bob).await?, 105 * STAG + 95 * STAG);
    assert_eq!(env.supply().await?, 301 * STAG);

    let status = env.view("get_status", json!({})).await?;
    assert_eq!(status["rounds_won"], 1);
    assert_eq!(status["rounds_lost"], 1);
    let rounds = env.view("get_rounds", json!({})).await?;
    assert_eq!(rounds.as_array().unwrap().len(), 2);
    assert_eq!(rounds[0]["won"], false);
    assert_eq!(rounds[1]["won"], true);
    assert_eq!(rounds[1]["minted"], (6 * STAG).to_string());

    let meta = env.view("ft_metadata", json!({})).await?;
    assert_eq!(meta["symbol"], "STAG");
    assert_eq!(meta["decimals"], 18);
    Ok(())
}

#[tokio::test]
async fn nobody_else_can_initialize() -> testresult::TestResult<()> {
    let sandbox = near_sandbox::Sandbox::start_sandbox().await?;
    let net = NetworkConfig::from_rpc_url("sandbox", sandbox.rpc_addr.parse()?);
    let signer = Signer::from_secret_key(
        near_sandbox::config::DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY.parse()?,
    )?;
    let contract = account(&sandbox, "stag.sandbox").await?;
    let mallory = account(&sandbox, "mallory.sandbox").await?;
    near_api::Contract::deploy(contract.clone())
        .use_code(wasm())
        .without_init_call()
        .with_signer(signer.clone())
        .send_to(&net)
        .await?
        .assert_success();
    let r = near_api::Contract(contract.clone())
        .call_function("new", json!({ "config": config(60) }))
        .transaction()
        .with_signer(mallory, signer.clone())
        .send_to(&net)
        .await?;
    assert!(r.into_result().is_err(), "a stranger must not be able to call new()");
    Ok(())
}
