#!/usr/bin/env python3
"""A standalone exact scalar model with no Enumeray or transport dependencies."""

import argparse
from decimal import Decimal, InvalidOperation
import sys


def double(value: Decimal) -> Decimal:
    """Double a finite decimal exactly, independently of the decimal context."""
    if not isinstance(value, Decimal) or not value.is_finite():
        raise ValueError("A finite decimal is required.")
    sign, digits, exponent = value.as_tuple()
    result = []
    carry = 0
    for digit in reversed(digits):
        carry, digit = divmod(digit * 2 + carry, 10)
        result.append(digit)
    if carry:
        result.append(carry)
    return Decimal((sign, tuple(reversed(result)), exponent))


def main() -> int:
    parser = argparse.ArgumentParser(description="Double one finite decimal exactly.")
    parser.add_argument("value", help="A decimal number, optionally in exponent notation")
    args = parser.parse_args()
    try:
        result = double(Decimal(args.value))
    except (InvalidOperation, ValueError, OverflowError):
        print("A finite decimal is required.", file=sys.stderr)
        return 1
    print(result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
