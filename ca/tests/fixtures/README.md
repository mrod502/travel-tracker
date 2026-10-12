# CA test fixtures

Test keys and pinned byte vectors for the `ca` crate. **These are test keys.**
Never point `[ca].key_path` at any file in this directory — they are committed to a
public repository, which makes them the opposite of a secret.

Regenerate the generated files with:

```sh
FIXTURES_UPDATE=1 cargo test -p ca --lib regenerate_fixtures -- --nocapture
```

(`root::tests::regenerate_fixtures`. Read the diff before committing: the point of
these files is that they *do not* change.)

## Representations, and where an encoding is allowed

A key, a signature or an identifier is **raw bytes** in the program and in the
database (`BYTEA`), and **a standard envelope** in a file: PKCS#8 PEM for a private
key, SPKI PEM for a public one. Inside a JSON document — which has no byte type —
the standard spelling is base64 (`ca::jsonbytes`). Hex appears in exactly two
places: a human reading an id (CLI output, error text), and these pinned vectors,
because a hex file is diffable and auditable in a way a binary blob is not.

## Files

| File | Written by | What it is |
|------|-----------|------------|
| `ca-root.pem` | the crate (`CaRoot::save_to_file`) | A CA root key, PKCS#8 v1 PEM (`-----BEGIN PRIVATE KEY-----`, RFC 5958 §2 / RFC 8410). Its `ca_id` is `bdbc2716f819fa33b1123940777860ed84747aeb8a93ae684d7df8887073bc51`. |
| `ca-root.pub.pem` | the crate (`TrustAnchor::save_to_file`) | The matching trust anchor: the same key's SPKI PEM (`-----BEGIN PUBLIC KEY-----`). This is what a node would be configured with. `openssl pkey -in ca-root.pem -pubout` reproduces it byte for byte. |
| `ca-golden.pem` | hand, then never touched | A second CA root key, PKCS#8 v1 PEM, whose only job is to be the issuer in the pinned preimage below. Its `ca_id` is `65b60673d6ed884bf01c2c222d82ada0740f29ac3355d6a925c81f17f47a27b8`, and the secret is `0x01..=0x20` — a fine test key and a terrible real one. `regenerate_fixtures` will not write it: a new key changes the id the signature covers, which is a decision, not a regeneration. |
| `ca-golden.preimage.hex` | **not the crate** | The golden: 194 bytes, exactly what a CA signs for the list described below. See *What the pinned preimage looks like*. |
| `rsl-seq1.json` | the crate | A published `RevocationStatusList`, sequence 1, signed by `ca-root.pem` — the JSON that would sit in the `revocation_status_lists` JSONB column. Keys, signatures and the issuer are base64 in it. |
| `rsl-seq1.preimage.hex` | the crate | The 194 bytes that signature covers, for cross-checking a rebuild. |

Two CAs rather than one, so that the pinned document and the published document are
signed by different keys: a change to the preimage builder then fails the pin without
also having to touch `rsl-seq1.json`, and the two vectors cannot drift into agreeing
with each other.

## What the pinned preimage looks like

All big-endian. Every length-prefixed field holds **the bytes of the thing itself**
— an identifier goes in as its 32 bytes, not as text.

```text
u32   32              issuer id length
32B   65b60673…       issuer id — SHA-256 of the CA public key
i64   1700000000      issued_at
i64   1700060480      expires_at
u64   7               sequence number
u32   2               entry count
  u32 32 + 32B        node id
  i64 1699900000      revoked_at
  u8  1               reason (KeyCompromise)          ← one byte, not four
  u32 32 + 32B        signing public key
  u32 32 + 32B        node id
  i64 1699999999      revoked_at
  u8  2               reason (CaCompromise)
  u32 0  + 0B         signing public key (absent)
```

Two things that look wrong and are not:

- **`node_id` is not `SHA-256(signing_public_key)` here.** Real entries satisfy that
  identity; these fixtures use `0x01`-repeated ids against `0x02`-repeated keys
  because the preimage pins the *encoding*, and a self-consistent pair would suggest
  the verifier checks it. It does not — that check is M14's territory.
- **`notes` is absent from the preimage entirely.** It is audit text, not signed, so
  editing it must not invalidate a published list.

## Why the golden is an input, not an output

`regenerate_fixtures` deliberately refuses to write `ca-golden.preimage.hex`.
Regenerating it would replace the pin with a copy of whatever the code does today,
and `the_rsl_preimage_is_pinned` would then pass against anything — including a
change that invalidates every list already in the database.

So the pinned bytes have to come from somewhere that is not the builder. Two
independent derivations were used, and both are reproducible:

```sh
# 1. The CA id, from the key file, with a tool this repo does not own.
openssl pkey -in ca-golden.pem -pubout -outform DER \
  | tail -c 32 | sha256sum          # → 65b60673…47a27b8

# 2. The whole document, assembled from the layout above by something that is
#    not ca::signing::build_rsl_payload.
python3 - <<'PY'
import hashlib, struct, subprocess, binascii
spki = subprocess.run(["openssl","pkey","-in","ca-golden.pem",
                       "-pubout","-outform","DER"], capture_output=True, check=True).stdout
ca_id  = hashlib.sha256(spki[-32:]).digest()
u32, u64 = lambda n: struct.pack(">I", n), lambda n: struct.pack(">Q", n)
entries = [(bytes([1])*32, 1_699_900_000, 1, bytes([2])*32),
           (binascii.unhexlify("aabbccddeeff00112233445566778899"*2),
            1_699_999_999, 2, b"")]
out = u32(len(ca_id)) + ca_id + u64(1_700_000_000) + u64(1_700_060_480) + u64(7)
out += u32(len(entries))
for nid, rev, reason, key in entries:
    out += u32(len(nid)) + nid + u64(rev) + bytes([reason]) + u32(len(key)) + key
print(out.hex())
PY
```

The file in this directory is where those two agree. The question the pin answers is
whether the *implementation* still matches the specification, and an answer taken
from the implementation cannot address that question — it has to come from a second
implementation, and the two agreeing is the whole evidence.
