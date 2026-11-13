> not formatted, will fix up later, just need to jot down stuff
## current
currently chose a btree because it already is apart of the standard library and is simple as a data 
structure to understand and use. 

## moving forward
will migrate to either a custom implementation or imported implementation of a [skip list](https://en.wikipedia.org/wiki/Skip_list)

## reasoning

Memtables take heavy, concurrent writes before flushing to disk, skip lists allow multiple threads to insert data simultaneously using simple atomic compare-and-swap operations. B-trees require node-locking during splits and rebalancing

LSM tree engines like [RocksDB](https://github.com/meta/rocksdb) and [LevelDB](https://github.com/google/leveldb) use skip lists
