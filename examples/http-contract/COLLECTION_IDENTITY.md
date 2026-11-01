# Collection identity, version 1

These rules describe the current version-1 contract. They are not RFC 8785/JCS.
Use a lossless JSON parser: binary floating point and JavaScript `JSON.parse`
can round scientific numbers before hashing. `collection-identity.py` uses only
the Python standard library and preserves number tokens. It needs no checkout,
installed CLI, credentials or service.

Acquire the [complete examples archive](https://doc.enumeray.com/examples/model-examples.tar.gz)
and its [checksum](https://doc.enumeray.com/examples/model-examples.tar.gz.sha256),
then use `examples/http-contract/` inside the extracted versioned root. Direct
downloads: [independent implementation](collection-identity.py),
[vector catalogue](digest-vectors/vectors.json), exact
[empty requested bytes](digest-vectors/empty-requested.bytes.json),
[two-member requested bytes](digest-vectors/mixed-requested.bytes.json),
[empty sealed bytes](digest-vectors/empty-sealed.bytes.json) and
[two-member sealed bytes](digest-vectors/mixed-sealed.bytes.json).

## Members and ordering

A requested manifest is a JSON array of complete member objects. Each object is
serialized in this exact field order: `path`, `media_type`, `scientific_metadata`,
`size_bytes`, `checksum_sha256`. A sealed manifest has those same fields followed
by `artifact_id`, then `version_id`; these pin the verified artifact and immutable
storage version. Collection-level metadata, IDs, member IDs, page boundaries and
availability flags are not part of either digest input.

Members must already be strictly increasing by path's Unicode scalar sequence
(equivalently UTF-8 byte order for valid Unicode). Paths must already be NFC;
reject non-NFC paths rather than normalize before hashing. Paths must also pass
the service's portable path, case-alias, prefix, size and declaration checks.
Sorting metadata keys does not reorder members or arrays.

All five requested-member fields are required. `scientific_metadata` is an
object: an absent field or `null` is invalid, while `{}` is valid. Inside metadata,
absent keys and keys with `null` values are different. Sort every metadata object
by Unicode scalar key order, recursively. Do not normalize metadata text or keys.
In particular non-BMP keys sort after U+E000, unlike UTF-16 code-unit order.

## Exact encoding and checksums

Serialize compact JSON: no indentation, spaces, BOM or trailing newline. Encode
the resulting text as UTF-8. Retain literal Unicode except U+0000–U+001F, quote
and backslash. Use `\b`, `\t`, `\n`, `\f`, `\r`, `\"` and `\\` for their
respective characters; other control characters use lowercase `\u00xx`.
Do not escape `/`, U+007F, U+2028 or non-ASCII characters unnecessarily.

Metadata number tokens follow the current arbitrary-precision parser: lowercase
the exponent marker and insert `+` if the exponent has no sign; preserve decimal
digits, trailing zeros and exponent digits. Integer `-0` becomes `0`. Thus `1`,
`1.0`, `1e0`, `1E+00` become `1`, `1.0`, `1e+0`, `1e+00`, with distinct digest
bytes. `-0.0` remains `-0.0`. Reject duplicate keys and non-JSON numbers.
`size_bytes` is a nonnegative integer serialized in base 10 without leading zeros.
The service's scientific JSON storage policy rejects NUL in strings/keys and
reserved serializer keys; serialization rules do not waive those admission checks.

Hash the exact serialized bytes with SHA-256. Encode all 32 digest bytes using
standard RFC 4648 Base64 with canonical padding, not URL-safe Base64 or hex.
This is the encoding of `manifest_sha256`, sealed `manifest_digest`, and member
`checksum_sha256`. A member checksum hashes its raw file bytes, not its metadata.
The requested manifest digest and sealed manifest digest differ once nonempty
members gain artifact/version pins. An empty manifest hashes the two bytes `[]`
in both cases. Completion preserves the sealed digest even if availability changes.

## Independently reproducible vectors

Download this guide, `collection-identity.py`, and the entire `digest-vectors/`
directory together. `vectors.json` identifies each source, exact `.bytes.json`
download, expected byte length and Base64 digest. The `.bytes.json` downloads have
no trailing newline; do not copy them through a text editor or Markdown block.
Both empty and mixed two-member requested/sealed manifests are included. The
mixed vectors cover nested keys, absent/null metadata, Unicode, control escapes,
numeric spelling, negative zero and numbers beyond IEEE-754 precision.

```sh
python3 collection-identity.py --verify-vectors digest-vectors
python3 collection-identity.py digest-vectors/mixed-requested.input.json \
  --bytes requested-digest-input.json
python3 collection-identity.py digest-vectors/mixed-sealed.input.json \
  --sealed --bytes sealed-digest-input.json
```

The request's `member_count`, `total_bytes` and requested digest must match the
whole ordered manifest, not only the current page. After freeze, bind completed,
verified member artifacts and seal. Retain both the original requested digest
and the sealed identity; never replace the original retry request with a sealed
manifest. See the adjacent HTTP workflow for complete transactions and replay.
