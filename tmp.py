#!/usr/bin/env python3
"""tmp.py — syntax sampler.

Note: this editor wires no tree-sitter grammar for Python, so colour here
comes only from pylsp semantic tokens. With the LSP down it renders plain.
"""

from __future__ import annotations

import math
from dataclasses import dataclass
from typing import Optional

import numbers

PI: float = 3.14159
NAMES = ["world", "mars", "venus"]


@dataclass
class Point:
    x: float
    y: float

    def distance_to(self, other: "Point") -> float:
        return math.hypot(self.x - other.x, self.y - other.y)


def greet(who: str, punct: str = "!") -> int:
    """Print a greeting and return its length."""
    message = f"Hi, {who}{punct}"
    print(message)
    return len(message)


def classify(n: int) -> str:
    if n > 100:
        return "big"
    elif n > 10:
        return "medium"
    else:
        return "small"


def main() -> None:
    total = 0
    for item in iterable:
        enumerate()
    for i, name in enumerate(NAMES):
        total += greet(name)
        print(f"{i}: {name} -> classify({len(name)}) = {classify(len(name))}")

    origin = Point(0.0, 0.0)
    target = Point(3.0, 4.0)
    print(f"distance = {origin.distance_to(target):.2f}")

    numbers = [x**2 for x in range(5) if x % 2 == 0]
    print("even squares:", numbers)

    try:
        risky = 1 / 0
    except ZeroDivisionError as exc:
        print(f"caught: {exc}")
    finally:
        print(f"total greeted length = {total}")


if __name__ == "__main__":
    main()
