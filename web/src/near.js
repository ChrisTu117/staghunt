// Minimal NEAR testnet helpers for the browser: views, a throwaway guest account
// created through the public testnet faucet, and signed calls with its key.
// The guest key never leaves this browser (localStorage).

import {
  Account,
  FailoverRpcProvider,
  JsonRpcProvider,
  KeyPair,
  KeyPairSigner,
  actions,
} from "near-api-js";

export const NETWORK = {
  rpcs: ["https://test.rpc.fastnear.com", "https://rpc.testnet.near.org"],
  faucet: "https://helper.nearprotocol.com/account",
  explorer: "https://testnet.nearblocks.io",
};

export const provider = new FailoverRpcProvider(
  NETWORK.rpcs.map((url) => new JsonRpcProvider({ url })),
);

const STORE_KEY = "staghunt.guest.v1";

export async function view(contractId, method, args = {}) {
  return provider.callFunction({ contractId, method, args });
}

export function loadGuest() {
  try {
    const raw = localStorage.getItem(STORE_KEY);
    return raw ? JSON.parse(raw) : null;
  } catch {
    return null;
  }
}

export function forgetGuest() {
  try {
    localStorage.removeItem(STORE_KEY);
  } catch {
    /* ignore */
  }
}

function randomName(prefix) {
  const bytes = crypto.getRandomValues(new Uint8Array(6));
  const tail = Array.from(bytes, (b) => (b % 36).toString(36)).join("");
  return `${prefix}-${tail}-${Date.now().toString(36)}.testnet`;
}

/** Create a new testnet account funded by the public faucet and keep its key here. */
export async function createGuest(prefix = "hunter", { remember = true } = {}) {
  const keyPair = KeyPair.fromRandom("ed25519");
  const accountId = randomName(prefix);
  const res = await fetch(NETWORK.faucet, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      newAccountId: accountId,
      newAccountPublicKey: keyPair.getPublicKey().toString(),
    }),
  });
  if (!res.ok) {
    throw new Error(`testnet faucet answered ${res.status}: ${await res.text()}`);
  }
  // wait until the account is visible to RPC
  for (let i = 0; i < 20; i++) {
    try {
      await provider.viewAccount({ accountId });
      break;
    } catch {
      await new Promise((r) => setTimeout(r, 1000));
    }
  }
  const guest = { accountId, secretKey: keyPair.toString() };
  if (remember) {
    try {
      localStorage.setItem(STORE_KEY, JSON.stringify(guest));
    } catch {
      /* private mode: the guest lives only for this page */
    }
  }
  return guest;
}

/**
 * Create `<name>-<random>.<parent>` funded with `yocto` from the parent guest. Used
 * for helper accounts so a visitor needs only one faucet account (the faucet limits
 * accounts per IP and day).
 */
export async function createSubGuest(parent, name, yocto) {
  const keyPair = KeyPair.fromRandom("ed25519");
  const tail = Array.from(crypto.getRandomValues(new Uint8Array(4)), (b) => (b % 36).toString(36)).join("");
  const accountId = `${name}-${tail}.${parent.accountId}`;
  const out = await accountFor(parent).signAndSendTransaction({
    receiverId: accountId,
    actions: [actions.createAccount(), actions.transfer(BigInt(yocto)), actions.addFullAccessKey(keyPair.getPublicKey())],
  });
  if (out?.status?.Failure) throw new Error(`creating ${accountId} failed`);
  for (const url of NETWORK.rpcs) {
    const p = new JsonRpcProvider({ url });
    for (let i = 0; i < 20; i++) {
      try {
        await p.viewAccessKey({ accountId, publicKey: keyPair.getPublicKey().toString() });
        break;
      } catch {
        await new Promise((r) => setTimeout(r, 1000));
      }
    }
  }
  return { accountId, secretKey: keyPair.toString() };
}

export function accountFor(guest) {
  const signer = new KeyPairSigner(KeyPair.fromString(guest.secretKey));
  return new Account(guest.accountId, provider, signer);
}

/** Signed function call; returns { result, txHash }. */
export async function call(guest, contractId, methodName, args, { deposit = "0", gasTgas = 100 } = {}) {
  const account = accountFor(guest);
  let outcome;
  try {
    outcome = await account.callFunctionRaw({
      contractId,
      methodName,
      args,
      deposit: BigInt(deposit),
      gas: BigInt(gasTgas) * 10n ** 12n,
    });
  } catch (e) {
    throw new Error(readable(e));
  }
  const txHash = outcome?.transaction?.hash ?? outcome?.transaction_outcome?.id;
  const status = outcome?.status ?? {};
  if (status.Failure) {
    const err = new Error(failureText(status.Failure));
    err.txHash = txHash;
    throw err;
  }
  let result = null;
  if (typeof status.SuccessValue === "string" && status.SuccessValue.length) {
    try {
      result = JSON.parse(atob(status.SuccessValue));
    } catch {
      result = null;
    }
  }
  return { result, txHash };
}

/** The RPC failover wraps the real error; show the first underlying one. */
export function readable(e) {
  const inner = Array.isArray(e?.errors) && e.errors.length ? e.errors[0] : e;
  const msg = inner?.message || String(inner);
  if (/NotEnoughBalance|Not enough balance/i.test(msg)) return "not enough testnet NEAR in this account for the amount plus gas";
  return msg;
}

function failureText(f) {
  const exec = f?.ActionError?.kind?.FunctionCallError?.ExecutionError;
  return exec ?? JSON.stringify(f);
}

export async function nearBalance(accountId) {
  const a = await provider.viewAccount({ accountId });
  return BigInt(a.amount);
}

export function txUrl(hash) {
  return `${NETWORK.explorer}/txns/${hash}`;
}

export function accountUrl(id) {
  return `${NETWORK.explorer}/address/${id}`;
}
