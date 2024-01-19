use criterion::{criterion_group, criterion_main, Criterion};
use kr2r::mmscanner::MinimizerScanner;

// 定义性能测试函数
fn performance_test(c: &mut Criterion) {
    let seq: Vec<u8> = b"ACGATCGACGACG".to_vec();
    let mut scanner = MinimizerScanner::default(seq, 5, 5);

    // 这里执行需要测试性能的操作，例如多次调用 next_minimizer
    c.bench_function("next_minimizer", |b| {
        b.iter(|| {
            let _ = scanner.next_minimizer();
        });
    });
}

// 创建性能测试组
criterion_group!(benches, performance_test);
criterion_main!(benches);
