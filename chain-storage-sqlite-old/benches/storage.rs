use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use rand_core::{OsRng, RngCore};

use chain_core::property::Block as _;
use chain_storage_sqlite_old::{test_utils::Block, BlockStore};

const BLOCK_DATA_LENGTH: usize = 1024;

fn criterion_benchmark(c: &mut Criterion) {
    let mut rng = OsRng;
    let mut block_data = [0; BLOCK_DATA_LENGTH];

    rng.fill_bytes(&mut block_data);
    let genesis_block = Block::genesis(Some(Box::new(block_data.clone())));

    let tempdir = tempfile::TempDir::new().unwrap();
    let path = {
        let mut path = tempdir.path().to_path_buf();
        path.push("test.sqlite");
        path
    };
    let store = BlockStore::file(path);
    let mut conn = store.connect::<Block>().unwrap();
    conn.put_block(&genesis_block).unwrap();

    let mut blocks = vec![genesis_block];

    c.bench_function("put_block", |b| {
        b.iter_batched(
            || {
                let last_block = blocks.get(rng.next_u32() as usize % blocks.len()).unwrap();
                rng.fill_bytes(&mut block_data);
                let block = last_block.make_child(Some(Box::new(block_data.clone())));
                blocks.push(block.clone());
                block
            },
            |block| conn.put_block(&block).unwrap(),
            BatchSize::PerIteration,
        )
    });

    c.bench_function("get_block", |b| {
        b.iter_batched(
            || {
                blocks
                    .get(rng.next_u32() as usize % blocks.len())
                    .unwrap()
                    .id()
            },
            |block_id| conn.get_block(&block_id).unwrap(),
            BatchSize::PerIteration,
        )
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
