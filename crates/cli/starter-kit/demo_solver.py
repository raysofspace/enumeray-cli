#!/usr/bin/env python3
"""A native command interface independent of the wrapper envelope."""
import argparse
from model import double, transform_csv
from protocol import MAX_FILE_BYTES, exact_json, publish, read_json


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=["json", "files"], required=True)
    parser.add_argument("--input", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    if args.mode == "json":
        content = (exact_json({"doubled": double(read_json(args.input)["value"])}) + "\n").encode()
        publish(args.output, content)
    else:
        _, content = transform_csv(args.input)
        publish(args.output, content, MAX_FILE_BYTES)


if __name__ == "__main__":
    main()
