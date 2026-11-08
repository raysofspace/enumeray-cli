# Collection identity, version 1

Use this reference to compute the version-1 digest for a collection manifest
independently. The requested digest covers the ordered members and their
metadata. The sealed digest also covers the exact artifact and version for each
member. Apply the encoding rules below before hashing. The contract uses its own
encoding rules, distinct from RFC 8785/JCS.

Use a lossless JSON parser so numbers keep their full precision and spelling
until serialization. Binary floating point and JavaScript `JSON.parse` can
round them before hashing. The included `collection-identity.py` preserves
number tokens and runs locally with the Python standard library.

Acquire the [complete examples archive](https://doc.enumeray.com/examples/model-examples.tar.gz)
and its [model examples archive checksum](https://doc.enumeray.com/examples/model-examples.tar.gz.sha256),
then use `examples/http-contract/` inside the extracted root. To inspect the
files individually, open the [independent implementation](collection-identity.py)
or the [vector catalogue](digest-vectors/vectors.json).

| Manifest | Requested bytes | Sealed bytes |
| --- | --- | --- |
| Empty | [Empty requested](digest-vectors/empty-requested.bytes.json) | [Empty sealed](digest-vectors/empty-sealed.bytes.json) |
| Two members | [Two-member requested](digest-vectors/mixed-requested.bytes.json) | [Two-member sealed](digest-vectors/mixed-sealed.bytes.json) |

## Members and ordering

A manifest is a JSON array of complete member objects. Write the keys of each
requested member in the following order.

1. `path`
2. `media_type`
3. `scientific_metadata`
4. `size_bytes`
5. `checksum_sha256`

For a sealed member, append `artifact_id` and then `version_id`. They pin
the verified artifact and immutable storage version. Omit collection-level
metadata, collection IDs, member IDs, page boundaries and availability flags
from both digest inputs.

Check that member paths are already in strictly increasing Unicode scalar
order. For valid Unicode, this equals UTF-8 byte order. Reject paths that are
not already NFC. The service also checks portability, case aliases, prefixes,
size and declarations. Keep member and array order as supplied.

Require all five requested-member fields. `scientific_metadata` must be an
object. Accept `{}` and reject an absent or `null` field. Inside the object,
keep an absent key distinct from a key with value `null`. Sort keys of every
metadata object, including nested objects, by Unicode scalar order. Keep
metadata text and keys in their original normalization. Non-BMP keys sort
after U+E000 in this order.

## Exact encoding and checksums

Encode compact JSON as UTF-8. Omit indentation, spacing outside string values,
a BOM and a trailing newline. Escape U+0000–U+001F, quotation marks and
backslashes. Use `\b`, `\t`, `\n`, `\f`, `\r`, `\"` and `\\` for their
respective characters. Use lowercase `\u00xx` for other control characters.
Write other characters literally, including `/`, U+007F, U+2028 and non-ASCII
characters.

For metadata number tokens, lowercase the exponent marker and insert `+` when
the exponent has no sign. Preserve decimal digits, trailing zeros and
exponent digits. Write integer `-0` as `0` and preserve `-0.0`. For example,
`1`, `1.0`, `1e0` and `1E+00` produce the distinct bytes `1`, `1.0`, `1e+0`
and `1e+00`. Reject duplicate keys and non-JSON numbers.
`size_bytes` is a nonnegative integer serialized in base 10 without leading zeros.
The service's scientific JSON storage policy rejects NUL in strings/keys and
reserved serializer keys; serialization rules do not waive those admission checks.

Hash the exact serialized bytes with SHA-256. Encode all 32 digest bytes using
standard RFC 4648 Base64 with canonical padding. Use that encoding for
`manifest_sha256`, sealed `manifest_digest` and member `checksum_sha256`. For
a member checksum, hash the raw file bytes.
The requested manifest digest and sealed manifest digest differ once nonempty
members gain artifact/version pins. An empty manifest hashes the two bytes `[]`
in both cases. Completion preserves the sealed digest even if availability changes.

## Independently reproducible vectors

Keep this guide, `collection-identity.py` and all of `digest-vectors/`
together. `vectors.json` lists each source, exact `.bytes.json` download,
byte length and Base64 digest. Download the byte files directly to preserve
their exact contents, including the absence of a trailing newline. The
two-member vectors cover nested keys, absent and null values, Unicode,
control escapes, numeric spelling, negative zero and numbers beyond IEEE-754
precision.

```sh
python3 collection-identity.py --verify-vectors digest-vectors
python3 collection-identity.py digest-vectors/mixed-requested.input.json \
  --bytes requested-digest-input.json
python3 collection-identity.py digest-vectors/mixed-sealed.input.json \
  --sealed --bytes sealed-digest-input.json
```

Compute `member_count`, `total_bytes` and the requested digest over the whole
ordered manifest. After freezing membership, bind verified member artifacts
and seal the collection. Save the original request and digest for retries.
Keep the sealed identity separately for the completed collection. See the
[HTTP workflow](HTTP_WORKFLOWS.md) for the full transaction.
