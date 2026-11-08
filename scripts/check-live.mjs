// Checks the shared testnet deployment against this repository:
//   npm run check:live              # code, parameters, metadata, status
//   npm run check:live -- --locked  # also require that the account has no access keys
//
// Claim: "the deployed contract is exactly res/staghunt.wasm with the parameters in
// deploy/testnet.json, and (once locked) nobody holds a key that could change it".
import { readFileSync } from "node:fs";
import { check, provider, sha256Base58, view } from "./lib.mjs";

const deploy = JSON.parse(readFileSync(new URL("../deploy/testnet.json", import.meta.url)));
const id = deploy.contract;
const wasm = readFileSync(new URL("../res/staghunt.wasm", import.meta.url));

console.log(`Checking ${id} on testnet`);
const acct = await provider.viewAccount({ accountId: id });
check(acct.code_hash === sha256Base58(wasm), `on-chain code hash ${acct.code_hash} = sha256(res/staghunt.wasm)`);
const cfg = await view(id, "get_config");
check(JSON.stringify(cfg) === JSON.stringify(deploy.config), "get_config() equals deploy/testnet.json");
const meta = await view(id, "ft_metadata");
check(meta.symbol === "STAG" && meta.decimals === 18, "ft_metadata: STAG, 18 decimals");
const s = await view(id, "get_status");
check(s.round_ends_at_ms > s.now_ms && s.round_ends_at_ms - s.now_ms <= cfg.round_secs * 1000, `hunt #${s.round} is open and closes within one round`);
check(s.target_bps >= cfg.min_target_bps && s.target_bps <= cfg.max_target_bps, `current target ${s.target_bps / 100}% is within bounds`);
const keys = await provider.viewAccessKeyList({ accountId: id });
const n = keys.keys.length;
console.log(`  info  ${id} has ${n} access key(s)`);
if (process.argv.includes("--locked")) check(n === 0, "contract account is locked (no access keys)");
console.log(process.exitCode ? "Some checks FAILED" : "All checks passed");
