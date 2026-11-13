# LSM Tree

A persistent key-value storage engine built from scratch in Rust, using a [Log-Structured Merge (LSM) tree](https://en.wikipedia.org/wiki/Log-structured_merge-tree) architecture.


## File Structure 
```
.
├── Cargo.toml
├── Cargo.lock
├── README.md
└── src/
    └── lib.rs       # memtable so far
```

## Testing

Run all tests:
```bash
cargo test
```
