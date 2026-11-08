// End-to-end demo on NEAR testnet, no setup needed besides `npm install`:
//
//   npm run demo
//
// 1. creates one fresh account through the public testnet faucet (or uses
//    SPONSOR=<your-account.testnet> from the near CLI credentials) and deploys
//    res/staghunt.wasm there with 30-second hunts (so the demo is quick and does not
//    depend on what other people did on the shared deployment);
// 2. creates three hunters, gives each 100 starter STAG;
// 3. hunt A: the three lock 30 + 20 + 10 = 60 STAG = 20% of supply -> success, +10%;
// 4. hunt B: one hunter locks 50 STAG alone -> fails (needs 3 hunters), -5% burned;
// 5. checks every balance and the supply against the rules and prints explorer links.
//
// Exit code 0 means every check passed.

import {
  STAG,
  accountLink,
  call,
  check,
  cleanup,
  demoRoot,
  deployWithInit,
  sleep,
  subAccount,
  stag,
  txLink,
  view,
} from "./lib.mjs";

const WASM = new URL("../res/staghunt.wasm", import.meta.url).pathname;
const ROUND_SECS = 30;
const NEAR = 10n ** 24n;

const config = {
  round_secs: ROUND_SECS,
  reward_bps: 1000,
  burn_bps: 500,
  start_target_bps: 2000,
  target_step_up_bps: 500,
  target_step_down_bps: 500,
  min_target_bps: 500,
  max_target_bps: 6000,
  min_hunters: 3,
  faucet_amount: (100n * STAG).toString(),
  faucet_closes_at_ms: Date.now() + 24 * 3600 * 1000,
};

async function waitForFreshRound(contractId, needSecs) {
  for (;;) {
    const s = await view(contractId, "get_status");
    const left = s.round_ends_at_ms - s.now_ms;
    if (left > needSecs * 1000) return s;
    process.stdout.write(`  … waiting ${Math.ceil(left / 1000)}s for hunt #${s.round + 1} to start\n`);
    await sleep(left + 1500);
  }
}

async function waitForClose(contractId, round) {
  for (;;) {
    const s = await view(contractId, "get_status");
    if (s.round > round) return s;
    const left = Math.max(0, s.round_ends_at_ms - s.now_ms);
    process.stdout.write(`  … hunt #${round} closes in ${Math.ceil(left / 1000)}s\n`);
    await sleep(left + 1500);
  }
}

const bal = async (c, a) => BigInt(await view(c, "ft_balance_of", { account_id: a }));
const created = { root: null, children: [] }; // for cleanup, also after a failure

async function main() {
  console.log("1. Fresh deployment");
  // one faucet account (or SPONSOR) pays for everything; hunters are its sub-accounts
  const owner = await demoRoot("stag-demo");
  created.root = owner;
  const contractId = owner.accountId;
  const deployTx = await deployWithInit(owner, WASM, "new", { config });
  console.log(`  contract ${contractId}  ${accountLink(contractId)}`);
  console.log(`  deploy+init tx ${txLink(deployTx)}`);

  console.log("2. Three hunters claim 100 starter STAG each");
  const hunters = [];
  for (const name of ["alice", "bob", "carol"]) {
    const a = await subAccount(owner, name, (3n * NEAR) / 10n);
    created.children.push(a);
    const r = await call(a, contractId, "claim_starter", {}, { deposit: 10n ** 22n });
    console.log(`  ${a.accountId} claimed  ${txLink(r.tx)}`);
    hunters.push(a);
  }
  const [alice, bob, carol] = hunters;
  check((await bal(contractId, alice.accountId)) === 100n * STAG, "alice holds 100 STAG");
  check(BigInt(await view(contractId, "ft_total_supply")) === 300n * STAG, "supply is 300 STAG");

  console.log("3. Hunt A: 30 + 20 + 10 = 60 STAG (target 20% of 300 = 60)");
  let s = await waitForFreshRound(contractId, 22);
  const roundA = s.round;
  // sent together so they land in the same hunt
  const joins = await Promise.all(
    [[alice, 30n], [bob, 20n], [carol, 10n]].map(async ([a, amt]) => {
      const r = await call(a, contractId, "join_hunt", { amount: (amt * STAG).toString() }, { deposit: 1n });
      return `  ${a.accountId} joined hunt #${r.value.stake_round} with ${amt}  ${txLink(r.tx)}`;
    }),
  );
  joins.forEach((line) => console.log(line));
  await sleep(2500); // reads use finalized state, a block or two behind
  s = await view(contractId, "get_status");
  check(s.round === roundA && s.hunters === 3 && BigInt(s.pot) === 60n * STAG, `hunt #${roundA} pot is 60 STAG from 3 hunters`);
  check(s.outlook === "on_track", "page would show: on track");
  s = await waitForClose(contractId, roundA);
  const a1 = await bal(contractId, alice.accountId);
  const b1 = await bal(contractId, bob.accountId);
  const c1 = await bal(contractId, carol.accountId);
  console.log(`  balances after hunt A: alice ${stag(a1)}, bob ${stag(b1)}, carol ${stag(c1)}`);
  check(a1 === 103n * STAG, "alice: 70 + 30 * 1.10 = 103");
  check(b1 === 102n * STAG, "bob: 80 + 20 * 1.10 = 102");
  check(c1 === 101n * STAG, "carol: 90 + 10 * 1.10 = 101");
  check(BigInt(await view(contractId, "ft_total_supply")) === 306n * STAG, "supply grew to 306 STAG");
  check(s.last_result && s.last_result.won === true && s.last_result.round === roundA, `get_status reports hunt #${roundA} won`);
  check(s.target_bps >= 2000, "target rose after the win (and only empty hunts lowered it since)");

  console.log("4. Hunt B: alice alone locks 50 STAG (needs 3 hunters)");
  s = await waitForFreshRound(contractId, 15);
  const roundB = s.round;
  const r = await call(alice, contractId, "join_hunt", { amount: (50n * STAG).toString() }, { deposit: 1n });
  console.log(`  alice joined hunt #${roundB} with 50  ${txLink(r.tx)}`);
  await waitForClose(contractId, roundB);
  const a2 = await bal(contractId, alice.accountId);
  console.log(`  alice after hunt B: ${stag(a2)}`);
  check(a2 === 53n * STAG + (475n * STAG) / 10n, "alice: 53 + 50 * 0.95 = 100.5");
  check(BigInt(await view(contractId, "ft_total_supply")) === 306n * STAG - (25n * STAG) / 10n, "supply fell to 303.5 STAG");

  console.log("5. Collect for real and re-check (views already counted it)");
  const coll = await call(alice, contractId, "collect_hunt", {});
  check(coll.value.status === "none" && BigInt(coll.value.balance) === 53n * STAG + (475n * STAG) / 10n, "collect_hunt keeps alice at 100.5");
  const rounds = await view(contractId, "get_rounds", {});
  check(rounds.length >= 2 && rounds[0].round === roundB && rounds[0].won === false, "history lists hunt B as failed");

  console.log(process.exitCode ? "\nSome checks FAILED" : "\nAll checks passed");
  if (await cleanup(owner, hunters)) {
    console.log(`Deleted the demo accounts; their NEAR went back to ${owner.sponsor}.`);
  } else {
    console.log(`Open the page against this deployment: docs/index.html?contract=${contractId}`);
  }
}

main().catch(async (e) => {
  console.error(e);
  if (created.root) await cleanup(created.root, created.children).catch(() => {});
  process.exit(1);
});
