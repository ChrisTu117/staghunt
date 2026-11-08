// Claim: "no owner, no admin method: the parameters can never change".
// Reads the ABI that cargo-near generated for res/staghunt.wasm and checks that the
// only state-changing methods are the game, NEP-141 and NEP-145 ones, and that the
// initializer is private (callable once, by the contract account itself).
import { readFileSync } from "node:fs";

const abi = JSON.parse(readFileSync(new URL("../res/staghunt_abi.json", import.meta.url)));
const fns = abi.body.functions;
const calls = fns.filter((f) => f.kind === "call").map((f) => f.name).sort();
const expected = [
  "claim_starter", "collect_hunt", "ft_resolve_transfer", "ft_transfer", "ft_transfer_call",
  "join_hunt", "new", "settle_rounds", "storage_deposit", "storage_unregister", "storage_withdraw",
].sort();
let ok = true;
const check = (cond, msg) => { console.log(`${cond ? "  ok  " : "  FAIL"}  ${msg}`); ok &&= cond; };

check(JSON.stringify(calls) === JSON.stringify(expected), `state-changing methods are exactly: ${expected.join(", ")}`);
if (JSON.stringify(calls) !== JSON.stringify(expected)) console.log("        found:", calls.join(", "));
const suspicious = fns.filter((f) => /owner|admin|set_|update|upgrade|migrate|pause|mint_to|withdraw_all/i.test(f.name));
check(suspicious.length === 0, "no method name looks like an admin control");
const init = fns.find((f) => f.name === "new");
check(init && init.modifiers?.includes("init") && init.modifiers?.includes("private"), "new() is an initializer and #[private]");
const resolver = fns.find((f) => f.name === "ft_resolve_transfer");
check(resolver?.modifiers?.includes("private"), "ft_resolve_transfer is #[private]");
process.exit(ok ? 0 : 1);
