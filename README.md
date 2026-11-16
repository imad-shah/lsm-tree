# LSM Tree

A persistent key-value storage engine built from scratch in Rust, using a [Log-Structured Merge (LSM) tree](https://en.wikipedia.org/wiki/Log-structured_merge-tree) architecture.


## File Structure 
```
.
├── Cargo.toml
├── Cargo.lock
├── README.md
├── decisions/
└── src/
    ├── lib.rs        # module declarations
    ├── engine.rs     # put/get/remove/scan, flushes the memtable to SSTables
    ├── entry.rs      # a value or a tombstone
    ├── memtable.rs   # in-memory sorted write buffer
    ├── merge.rs      # merges sorted sources, newest version of each key wins
    ├── record.rs     # on-disk record format, encode/decode
    └── sstable.rs    # writes and reads sorted on-disk tables
```

## Testing

Run all tests:
```bash
cargo test
```
