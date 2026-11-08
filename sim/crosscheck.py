"""Generate the cross-check scenario and the reference results from the Python model.

    python3 sim/crosscheck.py           # rewrite sim/crosscheck.json
    python3 sim/crosscheck.py --check   # exit 1 if sim/crosscheck.json is stale

The Rust unit test `crosscheck_with_python_model` replays every operation in the file
against the contract and compares balances and round state after each step.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from staghunt_sim import Config, Model  # noqa: E402

OUT = Path(__file__).resolve().parent / "crosscheck.json"
STAG = 10**18
ACCOUNTS = [f"p{i}" for i in range(6)]

CONFIG = Config(
    round_secs=30,
    reward_bps=1000,
    burn_bps=500,
    start_target_bps=2000,
    target_step_up_bps=500,
    target_step_down_bps=500,
    min_target_bps=500,
    max_target_bps=6000,
    min_hunters=2,
    faucet_amount=100 * STAG,
    faucet_closes_at_s=10**9,
)


class Lcg:
    """Same tiny generator as the Rust tests, so the scenario never depends on the
    Python version's `random` module."""

    def __init__(self, seed: int) -> None:
        self.s = seed

    def next(self) -> int:
        self.s = (self.s * 6364136223846793005 + 1442695040888963407) % 2**64
        return self.s >> 33

    def below(self, n: int) -> int:
        return 0 if n == 0 else self.next() % n


def build(seed: int = 2026, steps: int = 400) -> dict:
    m = Model(CONFIG)
    rng = Lcg(seed)
    ops = []
    for _ in range(steps):
        who = ACCOUNTS[rng.below(len(ACCOUNTS))]
        kind = rng.below(7)
        op = None
        if kind == 0 and who not in m.claimed:
            m.faucet(who)
            op = {"op": "faucet", "who": who}
        elif kind in (1, 2, 3) and m.balances.get(who, 0) > 0:
            bal = m.balances[who]
            # mostly round numbers, sometimes odd base units to hit rounding
            if rng.below(3) == 0:
                amount = 1 + rng.next() * 7919 % bal
            else:
                amount = max(1, bal * (1 + rng.below(100)) // 100)
            m.join(who, amount)
            op = {"op": "join", "who": who, "amount": str(amount)}
        elif kind == 4:
            to = ACCOUNTS[rng.below(len(ACCOUNTS))]
            if to != who and to in m.balances and m.balances.get(who, 0) > 0:
                amount = 1 + rng.next() * 104729 % m.balances[who]
                m.transfer(who, to, amount)
                op = {"op": "transfer", "from": who, "to": to, "amount": str(amount)}
        elif kind in (5, 6):
            secs = rng.below(75)
            m.advance(secs)
            op = {"op": "advance", "secs": secs}
        if op is not None:
            op["expect"] = m.snapshot(ACCOUNTS)
            ops.append(op)
    return {
        "generated_by": "sim/crosscheck.py",
        "seed": seed,
        "config": {
            "round_secs": CONFIG.round_secs,
            "reward_bps": CONFIG.reward_bps,
            "burn_bps": CONFIG.burn_bps,
            "start_target_bps": CONFIG.start_target_bps,
            "target_step_up_bps": CONFIG.target_step_up_bps,
            "target_step_down_bps": CONFIG.target_step_down_bps,
            "min_target_bps": CONFIG.min_target_bps,
            "max_target_bps": CONFIG.max_target_bps,
            "min_hunters": CONFIG.min_hunters,
            "faucet_amount": str(CONFIG.faucet_amount),
            "faucet_closes_at_s": CONFIG.faucet_closes_at_s,
        },
        "accounts": ACCOUNTS,
        "summary": {
            "ops": len(ops),
            "rounds_won": m.won,
            "rounds_lost": m.lost,
            "rounds_empty": m.empty,
        },
        "ops": ops,
    }


def main() -> int:
    data = json.dumps(build(), indent=1) + "\n"
    if "--check" in sys.argv:
        if not OUT.exists() or OUT.read_text() != data:
            print("sim/crosscheck.json is stale: run python3 sim/crosscheck.py")
            return 1
        print("sim/crosscheck.json is up to date")
        return 0
    OUT.write_text(data)
    s = json.loads(data)["summary"]
    print(f"wrote {OUT.name}: {s}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
