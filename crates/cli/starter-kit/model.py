"""Standalone scientific demo: exact scalar and CSV doubling. No Enumeray imports."""
import csv
from decimal import Decimal
import io
from pathlib import Path


def double(value):
    if not isinstance(value, Decimal) or not value.is_finite():
        raise ValueError("Finite decimal required")
    sign, digits, exponent = value.as_tuple()
    result, carry = [], 0
    for digit in reversed(digits):
        carry, digit = divmod(digit * 2 + carry, 10)
        result.append(digit)
    if carry:
        result.append(carry)
    return Decimal((sign, tuple(reversed(result)), exponent))


def transform_csv(path):
    with Path(path).open("rb") as source:
        raw = source.read(1048577)
    if len(raw) > 1048576:
        raise ValueError("Dataset too large")
    rows = list(csv.reader(io.StringIO(raw.decode("utf-8"), newline="")))
    if not rows or rows[0] != ["index", "value"] or not 1 <= len(rows) - 1 <= 10000:
        raise ValueError("Invalid CSV shape")
    output = io.StringIO(newline="")
    writer = csv.writer(output, lineterminator="\n")
    writer.writerow(["index", "value"])
    for index, row in enumerate(rows[1:]):
        if len(row) != 2 or row[0] != str(index):
            raise ValueError("Invalid CSV row")
        writer.writerow([index, str(double(Decimal(row[1])))])
    return len(rows) - 1, output.getvalue().encode("utf-8")
