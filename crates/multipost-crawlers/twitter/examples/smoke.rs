//! End-to-end smoke for TwitterCrawler. See toutiao/examples/smoke.rs.
use multipost_core::{CrawlOptions, Crawler};
use multipost_crawlers_twitter::TwitterCrawler;

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
    let items = TwitterCrawler::new().run(&opts).await?;
    println!("captured {} tweet(s):", items.len());
    for (i, it) in items.iter().enumerate() {
        let text = it
            .body
            .chars()
            .take(60)
            .collect::<String>()
            .replace('\n', " ");
        println!(
            "  [{:>3}] {} @{:<18} len={:<5} fav={:>5} rt={:>5} views={:>7} | {}",
            i + 1,
            it.item_id,
            it.author_handle.chars().take(18).collect::<String>(),
            it.body.chars().count(),
            it.metrics.like_count.unwrap_or(-1),
            it.metrics.share_count.unwrap_or(-1),
            it.metrics.view_count.unwrap_or(-1),
            text
        );
    }
    Ok(())
}
