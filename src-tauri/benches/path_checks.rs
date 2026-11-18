// ============================================================================
// Benchmarks for the path-check hot paths
//
// Covers the three functions named in section 8 of the architecture audit. All of
// them run once per file, so their cost scales linearly with file count:
//   deep_junk::could_be_junk_path      -- deep-scan pre-filter, must be allocation-free
//   deep_junk::normalize_into          -- deep-scan path normalization, reuses a buffer
//   delete_engine::is_protected_path   -- safety check during batch delete, once per file
//
// Running:
//   cargo bench                                     # everything
//   cargo bench -- path_checks/is_protected_path    # one group
//   cargo bench -- --save-baseline before           # record a baseline
//   cargo bench -- --baseline before                # compare against it
//
// Note: this only measures time, it asserts nothing. Correctness of these three
// functions lives in the #[test]s under src/. In particular is_protected_path is
// the fix site for audit finding C1 (path validation could be bypassed), and its
// regression assertions belong to the unit tests, not here.
// ============================================================================

use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;
use std::path::Path;

use cleanup_c_lib::bench_api::{could_be_junk_path, ensure_deletable, normalize_into, DeleteScope};

/// Pre-filter samples: half hits, half misses.
///
/// With only hits, the benchmark would measure nothing but the fast path that
/// matches on the first letter and report an optimistic number. Mixing in misses
/// is what reflects the real distribution.
const JUNK_PATH_SAMPLES: &[&str] = &[
    // Hit: common cache / temp directories
    r"C:\Users\Passacaglia\AppData\Local\Temp\setup.tmp",
    r"C:\Windows\Temp\install.log",
    r"C:\Users\Passacaglia\AppData\Local\Google\Chrome\User Data\Default\Cache\data_1",
    r"C:\$Recycle.Bin\S-1-5-21-1\$R4X8Q2.tmp",
    // Hit: non-ASCII path, to make sure byte-wise processing does not trip on it
    r"C:\Users\张三\AppData\Local\Temp\缓存\临时文件.tmp",
    // Miss: ordinary program and data directories
    r"C:\Program Files\SomeApp\bin\app.exe",
    r"D:\Games\SomeGame\data\pak0.pak",
    r"C:\Users\Passacaglia\Documents\报告.docx",
];

/// Normalization samples: mixed separators, mixed case, and one non-ASCII path.
const NORMALIZE_SAMPLES: &[&str] = &[
    r"C:\Users\Passacaglia\AppData\Local\Temp",
    r"C:/Users/Passacaglia/AppData/Local/Temp",
    r"c:\windows\system32\DRIVERS\etc",
    r"C:\Users\张三\AppData\Local\Temp\缓存",
];

/// Safety-check samples: protected paths, ordinary paths, and the three bypass
/// spellings listed in audit finding C1.
///
/// The bypass spellings are here on purpose: they must keep being classified as
/// protected. If the fix ever regresses to plain string-prefix matching, the
/// behaviour change shows up here first.
const PROTECTED_PATH_SAMPLES: &[&str] = &[
    // Must be blocked: critical system directories and files
    r"C:\Windows\System32\config\SAM",
    r"C:\Windows\System32\drivers\etc\hosts",
    r"C:\Program Files\SomeApp\app.dll",
    // Must be allowed: user documents and rebuildable caches
    r"C:\Users\Passacaglia\Documents\报告.docx",
    r"C:\Windows\Temp\install.log",
    // The three bypass spellings from C1: forward slashes, relative components,
    // extended-length prefix
    r"C:/Windows/System32/foo.dll",
    r"C:\Temp\..\Windows\System32\config",
    r"\\?\C:\Windows\System32\foo.dll",
];

/// Pre-filter: walks all samples in one pass, mirroring the per-file call pattern
/// of a deep scan.
fn bench_could_be_junk_path(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("path_checks");
    group.bench_function("could_be_junk_path", |bencher| {
        bencher.iter(|| {
            for path in JUNK_PATH_SAMPLES {
                black_box(could_be_junk_path(black_box(path)));
            }
        })
    });
    group.finish();
}

/// Normalization: the buffer is created outside the closure, exactly like a real
/// caller that reuses a single Vec.
fn bench_normalize_into(criterion: &mut Criterion) {
    let mut buffer: Vec<u8> = Vec::with_capacity(128);
    let mut group = criterion.benchmark_group("path_checks");
    group.bench_function("normalize_into", |bencher| {
        bencher.iter(|| {
            for path in NORMALIZE_SAMPLES {
                normalize_into(black_box(path), &mut buffer);
                // Read the result so the compiler cannot optimize the loop away
                black_box(buffer.len());
            }
        })
    });
    group.finish();
}

/// The full deletion gate: protection table + scope allowlist, once per file.
fn bench_ensure_deletable(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("path_checks");
    group.bench_function("ensure_deletable", |bencher| {
        bencher.iter(|| {
            for path in PROTECTED_PATH_SAMPLES {
                black_box(
                    ensure_deletable(Path::new(black_box(path)), DeleteScope::JunkClean).is_err(),
                );
            }
        })
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_could_be_junk_path,
    bench_normalize_into,
    bench_ensure_deletable
);
criterion_main!(benches);
