"""Independent reference model of the STAG rules.

The contract settles rounds lazily (on the next call) and keeps uncollected results in
aggregate counters. This model does the opposite: it closes every round eagerly at its
boundary and pays each hunter immediately, using plain Python integers. The two
implementations share no code; `crosscheck.py` replays the same operations on both and
the Rust test `crosscheck_with_python_model` asserts they agree to the last base unit.
"""

from __future__ import annotations

from dataclasses import dataclass, field

BPS = 10_000


@dataclass
class Config:
    round_secs: int
    reward_bps: int
    burn_bps: int
    start_target_bps: int
    target_step_up_bps: int
    target_step_down_bps: int
    min_target_bps: int
    max_target_bps: int
    min_hunters: int
    faucet_amount: int
    faucet_closes_at_s: int  # seconds after genesis


@dataclass
class Model:
    cfg: Config
    now: int = 0  # seconds since genesis
    balances: dict[str, int] = field(default_factory=dict)
    claimed: set[str] = field(default_factory=set)
    stakes: dict[str, int] = field(default_factory=dict)  # stakes in the open round
    round_id: int = 0
    target_bps: int = 0
    streak: int = 0
    best_streak: int = 0
    won: int = 0
    lost: int = 0
    empty: int = 0
    minted: int = 0
    burned: int = 0
    history: list[dict] = field(default_factory=list)

    def __post_init__(self) -> None:
        self.target_bps = self.cfg.start_target_bps

    # ----- helpers -----
    def supply(self) -> int:
        return sum(self.balances.values()) + sum(self.stakes.values())

    def _down(self, bps: int) -> int:
        return max(self.cfg.min_target_bps, bps - self.cfg.target_step_down_bps)

    def _up(self, bps: int) -> int:
        return min(self.cfg.max_target_bps, bps + self.cfg.target_step_up_bps)

    def _close_round(self) -> None:
        pot = sum(self.stakes.values())
        if pot == 0:
            self.empty += 1
            self.streak = 0
            self.target_bps = self._down(self.target_bps)
        else:
            supply = self.supply()
            # ceil(target_bps * supply / BPS) without floats
            target = -((-self.target_bps * supply) // BPS)
            success = len(self.stakes) >= self.cfg.min_hunters and pot >= target
            delta = 0
            for who, stake in self.stakes.items():
                if success:
                    bonus = stake * self.cfg.reward_bps // BPS
                    self.balances[who] += stake + bonus
                    delta += bonus
                else:
                    burn = stake * self.cfg.burn_bps // BPS
                    self.balances[who] += stake - burn
                    delta += burn
            prev_bps = self.target_bps
            if success:
                self.won += 1
                self.minted += delta
                self.streak += 1
                self.best_streak = max(self.best_streak, self.streak)
                self.target_bps = self._up(self.target_bps)
            else:
                self.lost += 1
                self.burned += delta
                self.streak = 0
                self.target_bps = self._down(self.target_bps)
            self.history.append(
                {
                    "round": self.round_id,
                    "pot": pot,
                    "hunters": len(self.stakes),
                    "target_bps": prev_bps,
                    "target": target,
                    "supply_at_close": supply,
                    "won": success,
                    "delta": delta,
                }
            )
        self.stakes = {}
        self.round_id += 1

    # ----- operations -----
    def advance(self, secs: int) -> None:
        end = self.now + secs
        while (self.round_id + 1) * self.cfg.round_secs <= end:
            self._close_round()
        self.now = end

    def faucet(self, who: str) -> None:
        if self.now >= self.cfg.faucet_closes_at_s:
            raise ValueError("the faucet is closed")
        if who in self.claimed:
            raise ValueError("already claimed")
        self.claimed.add(who)
        self.balances[who] = self.balances.get(who, 0) + self.cfg.faucet_amount

    def join(self, who: str, amount: int) -> None:
        if amount <= 0 or self.balances.get(who, 0) < amount:
            raise ValueError("not enough balance")
        self.balances[who] -= amount
        self.stakes[who] = self.stakes.get(who, 0) + amount

    def transfer(self, src: str, dst: str, amount: int) -> None:
        if src == dst or amount <= 0 or self.balances.get(src, 0) < amount:
            raise ValueError("bad transfer")
        if dst not in self.balances:
            raise ValueError("receiver not registered")
        self.balances[src] -= amount
        self.balances[dst] += amount

    def snapshot(self, accounts: list[str]) -> dict:
        return {
            "balances": {a: str(self.balances.get(a, 0)) for a in accounts},
            "pot": str(sum(self.stakes.values())),
            "supply": str(self.supply()),
            "round": self.round_id,
            "target_bps": self.target_bps,
            "streak": self.streak,
            "best_streak": self.best_streak,
            "rounds_won": self.won,
            "rounds_lost": self.lost,
            "rounds_empty": self.empty,
            "total_minted": str(self.minted),
            "total_burned": str(self.burned),
        }
