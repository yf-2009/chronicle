# Chronicle

A fault-tolerant, replicated key-value database, built from scratch in Rust.

**Status: early / in progress.** This README will grow into full
documentation (architecture, consistency model, benchmarks with real
measured numbers, and a captured chaos-failover demo) as the corresponding
pieces land. Right now the storage engine is up: a write-ahead log, an
in-memory table, and an SSTable/compaction layer, all with crash-recovery
tests.

## What's here so far

- `crates/chronicle-storage` -- the LSM-tree storage engine:
  - `wal`: CRC32-framed, append-only write-ahead log
  - `memtable`: sorted in-memory table (see `docs/adr/0002` for why this is
    a `BTreeMap` rather than a hand-rolled skip list)
  - `sstable`: immutable, sorted on-disk tables with a sparse index
  - `bloom`: Bloom filter for negative lookups
  - `compaction`: merges SSTables, keeping the newest version of each key
  - `engine`: `LsmEngine`, wiring the above into a durable KV store with
    crash recovery from the WAL

## Roadmap

See `docs/roadmap.md` once it lands for the full plan: MVCC with snapshot
isolation, a hand-rolled Raft consensus implementation (no external Raft
crate), a real TCP-based replicated server and CLI client, a chaos-testing
harness with a captured leader-failover demo, a `/metrics` endpoint, and
honestly-labeled benchmarks.

## Building

```
cargo build --workspace
cargo test --workspace
```

## Development notes

This project is built and verified without a local Rust toolchain in the
loop -- see `docs/adr/0006-remote-ci-verification.md` for why, and what that
does and doesn't change about how the code was written and checked.
