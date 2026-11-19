# LSM Tree

A persistent key-value storage engine built from scratch in Rust, using a [Log-Structured Merge (LSM) tree](https://en.wikipedia.org/wiki/Log-structured_merge-tree) architecture.

Every write gets a sequence number and is kept as a new version, so reads can see the latest data or the data as of an earlier sequence number ([MVCC](https://en.wikipedia.org/wiki/Multiversion_concurrency_control)).

## File Structure
```
.
├── Cargo.toml
├── Cargo.lock
├── README.md
├── decisions/
└── src/
    ├── lib.rs        # module declarations
    ├── engine.rs     # put/get/remove/scan, flush, compaction with version cleanup
    ├── entry.rs      # a value or a tombstone
    ├── keys.rs       # internal key (user key + sequence number) and its ordering
    ├── memtable.rs   # in-memory sorted write buffer
    ├── merge.rs      # merges sorted sources into one stream of all versions
    ├── visible.rs    # picks the version of each key visible at a sequence number
    ├── record.rs     # on-disk record format, encode/decode
    └── sstable.rs    # writes and reads sorted on-disk tables
```

## Testing

Run all tests:
```bash
cargo test
```
