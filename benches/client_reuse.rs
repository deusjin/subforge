//! HTTP client reuse benchmark.
//!
//! Run with: `cargo bench --bench client_reuse`
//!
//! Demonstrates the latency cost of creating a fresh `reqwest::Client` per
//! request (TLS handshake + connection pool reset) vs reusing a long-lived
//! client. The translate path uses a process-wide `OnceLock<Client>` for
//! exactly this reason; this bench is the empirical justification.

use std::time::Instant;

const N: usize = 10;
const URL: &str = "https://translate.google.com/m?sl=en&tl=zh-CN&q=hello";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // Warm up DNS so the first call doesn't dominate the timing.
    let _ = reqwest::get(URL).await;

    // --- New Client per request ---
    let start = Instant::now();
    for _ in 0..N {
        let client = reqwest::Client::new();
        let _ = client
            .get(URL)
            .header("User-Agent", "Mozilla/4.0")
            .send()
            .await;
    }
    let new_elapsed = start.elapsed();

    // --- Shared Client ---
    let client = reqwest::Client::new();
    let start = Instant::now();
    for _ in 0..N {
        let _ = client
            .get(URL)
            .header("User-Agent", "Mozilla/4.0")
            .send()
            .await;
    }
    let shared_elapsed = start.elapsed();

    let new_per = new_elapsed.as_millis() / N as u128;
    let shared_per = shared_elapsed.as_millis() / N as u128;
    let savings_ms = new_elapsed.as_millis() as i128 - shared_elapsed.as_millis() as i128;

    println!("\n=== Client Reuse Benchmark ({N} sequential HTTPS requests) ===");
    println!(
        "New Client each:  {:>6}ms total, {:>4}ms/req",
        new_elapsed.as_millis(),
        new_per
    );
    println!(
        "Shared Client:    {:>6}ms total, {:>4}ms/req",
        shared_elapsed.as_millis(),
        shared_per
    );
    let pct = if new_elapsed.as_millis() > 0 {
        savings_ms as f64 / new_elapsed.as_millis() as f64 * 100.0
    } else {
        0.0
    };
    println!(
        "Savings:          {:>6}ms total ({:.0}% faster)",
        savings_ms, pct
    );
    println!(
        "Extrapolated to 100 cues: ~{}ms saved",
        savings_ms * 100 / N as i128
    );
}
