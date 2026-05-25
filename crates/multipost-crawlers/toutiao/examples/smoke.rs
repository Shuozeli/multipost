//! End-to-end smoke test: runs ToutiaoCrawler against the live feed
//! and prints a one-line summary per item.
//!
//! Requires: pwright in PATH, Chrome reachable at $PWRIGHT_CDP (or
//! pwright's default localhost:9222), and the active Chrome session
//! logged into toutiao.com.
//!
//! Run with:
//!   PWRIGHT_CDP=http://localhost:9222 \
//!     cargo run --example smoke -p multipost-crawlers-toutiao -- 30

use multipost_core::{CrawlOptions, Crawler};
use multipost_crawlers_toutiao::ToutiaoCrawler;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let duration: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let opts = CrawlOptions {
        duration_secs: duration,
        ..Default::default()
    };
    let crawler = ToutiaoCrawler::new();
    let items = crawler.run(&opts).await?;
    println!("captured {} item(s):", items.len());
    for (i, it) in items.iter().enumerate() {
        let title = it.body.chars().take(50).collect::<String>();
        println!(
            "  [{:>3}] read={:>6} like={:>4} cmt={:>4} sh={:>3} bm={:>3} | {:<14} | {}",
            i + 1,
            it.metrics.read_count.unwrap_or(-1),
            it.metrics.like_count.unwrap_or(-1),
            it.metrics.comment_count.unwrap_or(-1),
            it.metrics.share_count.unwrap_or(-1),
            it.metrics.bookmark_count.unwrap_or(-1),
            it.author_handle.chars().take(14).collect::<String>(),
            title,
        );
    }
    Ok(())
}
