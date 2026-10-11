# Vendored `mssql-tds-preview`

This directory contains the crates.io source for `mssql-tds-preview` version
`0.1.0-preview.1`. The original crate archive SHA-256 is
`454957bed59024ea56acb311e8c0b74369182f1a3256d9e036d4b46f687d0462`, matching
the checksum recorded in the repository's original `Cargo.lock` entry. The
crate's source and manifest are otherwise copied from that archive. The crate
manifest declares MIT; the registry archive omits the license file, so
`LICENSE` is copied from the upstream repository at commit
`d43bdcc2f2d6f16155a259e2db7240365e6271df` (`mssql-tds/LICENSE`). Its SHA-256
is `32a86af620befcdd1a9c973ee653ac2bc0932a44224b1e1c2215ccd2e9ec9dc1`.

Local patch:

- `src/connection/transport/network_transport.rs` propagates token-read errors
  while waiting for an ATTENTION acknowledgment. It reports success only after
  receiving a `DONE` token with the `ATTN` status bit. EOF, malformed input, and
  non-ATTN DONE tokens never count as confirmation.
- `src/core.rs` adds a shared `Arc<AtomicBool>` witness to `CancelHandle`. Child
  handles share the witness, which the network transport sets only after it
  reads a real `DONE/ATTN` acknowledgment while handling
  `OperationCancelledError`. Packet-write interruption and timeout do not set
  it. The caller must also check the typed TDS error and its own cancellation
  state before reporting confirmed cancellation.
- Focused tests cover real `DONE/ATTN`, closed response, non-ATTN DONE, and
  cancellation during packet writing.

`src/io/packet_writer.rs` differs only by the packet-write cancellation
regression test; it verifies that an interrupted write leaves the ACK witness
false.

The archive's `.env` and crate-local `Cargo.lock` are intentionally omitted;
they are not needed to build this dependency and may contain local settings.
