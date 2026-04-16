//! Per-stage ingest benchmark. Uses only public crate APIs.
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let shard_path: PathBuf = std::env::args().nth(1).expect("shard.git").into();
    let data_dir: PathBuf = std::env::args().nth(2).expect("data_dir").into();
    let limit: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5000);

    std::fs::create_dir_all(&data_dir).unwrap();
    eprintln!(
        "shard: {} | data_dir: {} | limit: {limit}",
        shard_path.display(),
        data_dir.display()
    );

    let mut repo = gix::open(&shard_path).expect("gix open");
    repo.object_cache_size(256 * 1024 * 1024);
    let head_id = repo.head_id().expect("head_id").detach();
    let walk = repo.rev_walk([head_id]).all().expect("rev_walk");

    let store = _core::Store::open(&data_dir, "bench").expect("store open");

    let mut t_git = 0u64; // walk + tree + blob
    let mut t_parse = 0u64; // mail_parser
    let mut t_store = 0u64; // zstd + append
    let mut count = 0usize;
    let mut total_bytes = 0u64;
    let mut parse_failures = 0u64;

    let t_total = Instant::now();

    for info in walk {
        if count >= limit {
            break;
        }

        let t0 = Instant::now();
        let info = info.expect("walk");
        let commit = info.object().expect("commit");
        let tree = commit.tree().expect("tree");
        let Some(entry) = tree.find_entry("m") else {
            continue;
        };
        let blob = entry.object().expect("blob");
        let data = blob.data.clone();
        total_bytes += data.len() as u64;
        t_git += t0.elapsed().as_nanos() as u64;

        let t1 = Instant::now();
        // Use mail_parser directly since parse module is private
        let msg = mail_parser::MessageParser::default().parse(&data);
        if msg.is_none() {
            parse_failures += 1;
        }
        t_parse += t1.elapsed().as_nanos() as u64;

        let t2 = Instant::now();
        let _ = store.append(&data).expect("store");
        t_store += t2.elapsed().as_nanos() as u64;

        count += 1;
    }

    let total_ms = t_total.elapsed().as_millis();
    let ms = |ns: u64| ns as f64 / 1_000_000.0;

    eprintln!(
        "\n=== {count} messages, {total_ms}ms total ({:.2} ms/msg) ===",
        total_ms as f64 / count as f64
    );
    eprintln!(
        "  git (walk+tree+blob): {:>8.1}ms  ({:.3} ms/msg)  {:>5.1}%",
        ms(t_git),
        ms(t_git) / count as f64,
        ms(t_git) / total_ms as f64 * 100.0
    );
    eprintln!(
        "  mail_parser parse:    {:>8.1}ms  ({:.3} ms/msg)  {:>5.1}%",
        ms(t_parse),
        ms(t_parse) / count as f64,
        ms(t_parse) / total_ms as f64 * 100.0
    );
    eprintln!(
        "  zstd+store append:    {:>8.1}ms  ({:.3} ms/msg)  {:>5.1}%",
        ms(t_store),
        ms(t_store) / count as f64,
        ms(t_store) / total_ms as f64 * 100.0
    );
    eprintln!(
        "  data: {} MB | parse failures: {}",
        total_bytes / 1024 / 1024,
        parse_failures
    );
}
