use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::hint::black_box;
use std::time::Instant;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug)]
struct BenchRow {
    case: &'static str,
    payload_bytes: usize,
    iterations: u64,
    total_ms: f64,
    ops_per_s: f64,
}

fn generic_signature_hex(secret: &[u8], body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("valid HMAC key");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

fn payload_hash_hex(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    hex::encode(hasher.finalize())
}

fn make_payload(size: usize) -> Vec<u8> {
    if size <= 64 {
        return "x".repeat(size).into_bytes();
    }

    let filler = "x".repeat(size - 64);
    format!(
        "{{\"event\":\"perf.webhook.received\",\"data\":{{\"filler\":\"{}\"}}}}",
        filler
    )
    .into_bytes()
}

fn run_case(
    case: &'static str,
    payload_bytes: usize,
    iterations: u64,
    mut f: impl FnMut(),
) -> BenchRow {
    let started = Instant::now();
    for _ in 0..iterations {
        f();
    }
    let elapsed = started.elapsed();
    let total_ms = elapsed.as_secs_f64() * 1000.0;
    let ops_per_s = iterations as f64 / elapsed.as_secs_f64();

    BenchRow {
        case,
        payload_bytes,
        iterations,
        total_ms,
        ops_per_s,
    }
}

fn parse_iterations() -> u64 {
    std::env::args()
        .nth(1)
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(200_000)
}

fn main() {
    let iterations = parse_iterations();
    let secret = b"dev-generic-secret";
    let sizes = [256usize, 1024, 4096];

    let mut rows = Vec::new();

    for size in sizes {
        let payload = make_payload(size);
        rows.push(run_case("hmac_signature_hex", size, iterations, || {
            black_box(generic_signature_hex(
                black_box(secret),
                black_box(payload.as_slice()),
            ));
        }));

        rows.push(run_case("payload_hash_hex", size, iterations, || {
            black_box(payload_hash_hex(black_box(payload.as_slice())));
        }));
    }

    println!("# Webhook Hot Path Bench");
    println!(
        "# iterations_per_case={} payload_sizes=256,1024,4096",
        iterations
    );
    println!("| Case | Payload bytes | Iterations | Total time (ms) | Ops/s |");
    println!("|---|---:|---:|---:|---:|");

    for row in rows {
        println!(
            "| {} | {} | {} | {:.2} | {:.2} |",
            row.case, row.payload_bytes, row.iterations, row.total_ms, row.ops_per_s
        );
    }
}
