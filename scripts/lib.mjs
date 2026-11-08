// Shared helpers for the Node scripts (testnet only). No keys are read from or
// written to the repository: demo accounts are created through the public testnet
// faucet and their keys live in memory for the duration of the script.

import {
  Account,
  FailoverRpcProvider,
  JsonRpcProvider,
  KeyPair,
  KeyPairSigner,
  actions,
} from "near-api-js";
import { existsSync, mkdirSync, readFileSync, unlinkSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { createHash } from "node:crypto";

export const RPCS = ["https://test.rpc.fastnear.com", "https://rpc.testnet.near.org"];
export const FAUCET = "https://helper.nearprotocol.com/account";
export const EXPLORER = "https://testnet.nearblocks.io";
export const STAG = 10n ** 18n;
export const TGAS = 10n ** 12n;

export const provider = new FailoverRpcProvider(RPCS.map((url) => new JsonRpcProvider({ url })));

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export function randomSuffix() {
  return Math.random().toString(36).slice(2, 8) + Date.now().toString(36).slice(-4);
}

/** Ask the public testnet faucet for a new funded account (10 testnet NEAR). */
export async function faucetAccount(accountId) {
  const keyPair = KeyPair.fromRandom("ed25519");
  for (let attempt = 1; ; attempt++) {
    const res = await fetch(FAUCET, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ newAccountId: accountId, newAccountPublicKey: keyPair.getPublicKey().toString() }),
    });
    if (res.ok) break;
    const text = await res.text();
    if (attempt >= 4) throw new Error(`faucet refused ${accountId}: ${res.status} ${text}`);
    await sleep(3000 * attempt);
  }
  for (let i = 0; i < 30; i++) {
    try {
      await provider.viewAccount({ accountId });
      break;
    } catch {
      await sleep(1000);
    }
  }
  return new Account(accountId, provider, new KeyPairSigner(keyPair));
}

/**
 * The account that pays for a demo run. By default a fresh account from the public
 * testnet faucet (rate-limited per IP: a few dozen accounts a day). With
 * SPONSOR=<your-account.testnet> it uses that account's key from the near CLI
 * credentials folder (~/.near-credentials/testnet/<id>.json) instead. Nothing is
 * written anywhere.
 */
export async function demoRoot(prefix) {
  const sponsor = process.env.SPONSOR;
  if (!sponsor) return faucetAccount(`${prefix}-${randomSuffix()}.testnet`);
  const file = `${homedir()}/.near-credentials/testnet/${sponsor}.json`;
  if (!existsSync(file)) throw new Error(`SPONSOR=${sponsor} but ${file} does not exist`);
  const { private_key } = JSON.parse(readFileSync(file, "utf8"));
  const parent = new Account(sponsor, provider, new KeyPairSigner(KeyPair.fromString(private_key)));
  const root = await subAccount(parent, `${prefix}-${randomSuffix()}`, 45n * 10n ** 23n);
  root.sponsor = sponsor;
  // keep the key where near CLI keeps keys, so a crashed run can be cleaned up by hand:
  //   near account delete-account <root> beneficiary <sponsor> ... sign-with-legacy-keychain
  const dir = `${homedir()}/.near-credentials/testnet`;
  mkdirSync(dir, { recursive: true });
  writeFileSync(
    `${dir}/${root.accountId}.json`,
    JSON.stringify({ account_id: root.accountId, public_key: root.publicKey, private_key: root.secretKey }),
    { mode: 0o600 },
  );
  return root;
}

/**
 * In SPONSOR mode, delete the demo accounts afterwards so the sponsor gets its NEAR
 * back (set KEEP=1 to keep the deployment for a look in the browser). Faucet-made
 * demo deployments are kept: they cost nothing.
 */
export async function cleanup(root, children) {
  if (!root.sponsor || process.env.KEEP) return false;
  for (const c of children) {
    try {
      await c.deleteAccount(root.accountId);
    } catch (e) {
      console.error(`  could not delete ${c.accountId}: ${e.message}`);
    }
  }
  await root.deleteAccount(root.sponsor);
  try {
    unlinkSync(`${homedir()}/.near-credentials/testnet/${root.accountId}.json`);
  } catch {
    /* already gone */
  }
  return true;
}

/** Create `<name>.<parent>` with a fresh key and `yocto` NEAR from the parent. */
export async function subAccount(parent, name, yocto) {
  const keyPair = KeyPair.fromRandom("ed25519");
  const accountId = `${name}.${parent.accountId}`;
  const out = await parent.signAndSendTransaction({
    receiverId: accountId,
    actions: [actions.createAccount(), actions.transfer(yocto), actions.addFullAccessKey(keyPair.getPublicKey())],
  });
  if (out?.status?.Failure) throw new Error(`creating ${accountId} failed: ${JSON.stringify(out.status.Failure)}`);
  await waitForKey(accountId, keyPair.getPublicKey().toString());
  const account = new Account(accountId, provider, new KeyPairSigner(keyPair));
  account.publicKey = keyPair.getPublicKey().toString();
  account.secretKey = keyPair.toString();
  return account;
}

/** Both public RPCs must see a new key before we sign with it. */
async function waitForKey(accountId, publicKey) {
  for (const url of RPCS) {
    const p = new JsonRpcProvider({ url });
    for (let i = 0; i < 30; i++) {
      try {
        await p.viewAccessKey({ accountId, publicKey });
        break;
      } catch {
        await sleep(1000);
      }
    }
  }
}

export async function view(contractId, method, args = {}) {
  return provider.callFunction({ contractId, method, args });
}

/** Function call that throws with the contract's panic message on failure. */
export async function call(account, contractId, methodName, args = {}, { deposit = 0n, gas = 100n * TGAS } = {}) {
  const out = await account.callFunctionRaw({ contractId, methodName, args, deposit, gas });
  const failure = out?.status?.Failure;
  if (failure) {
    const msg = failure?.ActionError?.kind?.FunctionCallError?.ExecutionError ?? JSON.stringify(failure);
    const err = new Error(msg);
    err.tx = out?.transaction?.hash;
    throw err;
  }
  return { tx: out?.transaction?.hash, value: decodeValue(out?.status?.SuccessValue), outcome: out };
}

function decodeValue(b64) {
  if (!b64) return null;
  try {
    return JSON.parse(Buffer.from(b64, "base64").toString());
  } catch {
    return null;
  }
}

/** Deploy `wasmPath` to `account` and call `init(args)` in the same transaction. */
export async function deployWithInit(account, wasmPath, initMethod, initArgs) {
  const code = readFileSync(wasmPath);
  const out = await account.signAndSendTransaction({
    receiverId: account.accountId,
    actions: [
      actions.deployContract(code),
      actions.functionCall(initMethod, initArgs, 100n * TGAS, 0n),
    ],
  });
  if (out?.status?.Failure) throw new Error(`deploy failed: ${JSON.stringify(out.status.Failure)}`);
  return out?.transaction?.hash;
}

export function sha256Base58(bytes) {
  return base58(createHash("sha256").update(bytes).digest());
}

const ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
export function base58(buf) {
  let n = BigInt("0x" + Buffer.from(buf).toString("hex"));
  let s = "";
  while (n > 0n) {
    s = ALPHABET[Number(n % 58n)] + s;
    n /= 58n;
  }
  for (const b of buf) {
    if (b === 0) s = "1" + s;
    else break;
  }
  return s;
}

export function stag(base) {
  const v = BigInt(base);
  const whole = v / STAG;
  const frac = ((v % STAG) * 1000n) / STAG;
  return `${whole}.${frac.toString().padStart(3, "0")}`;
}

export const txLink = (h) => `${EXPLORER}/txns/${h}`;
export const accountLink = (a) => `${EXPLORER}/address/${a}`;

export function check(cond, label) {
  if (!cond) {
    console.error(`  FAIL  ${label}`);
    process.exitCode = 1;
    return false;
  }
  console.log(`  ok    ${label}`);
  return true;
}
