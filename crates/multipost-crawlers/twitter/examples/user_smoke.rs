//! End-to-end smoke for TwitterUserCrawler against a live Chrome.
//!
//! Usage:
//!   PWRIGHT_CDP=http://alienware-win-yuacx.tail8f3b66.ts.net:9222 \
//!   PWRIGHT_BIN=/path/to/pwright \
//!   cargo run -p multipost-crawlers-twitter --example user_smoke -- Tesla 25
use multipost_core::{UserCrawlOptions, UserCrawler};
use multipost_crawlers_twitter::TwitterUserCrawler;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handle = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Tesla".to_string());
    let max: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(25);
    let opts = UserCrawlOptions {
        handle: handle.clone(),
        max_posts: max,
        max_duration_secs: 90,
        ..Default::default()
    };
    println!("crawling @{handle} (max {max}) via {:?}", opts.cdp_url);
    let items = TwitterUserCrawler::new().crawl_user(&opts).await?;
    println!("captured {} post(s):", items.len());
    for (i, it) in items.iter().enumerate() {
        let text = it
            .body
            .chars()
            .take(60)
            .collect::<String>()
            .replace('\n', " ");
        println!(
            "  [{:>3}] @{:<16} fav={:>6} rt={:>5} reply={:>4} views={:>8} | {}",
            i + 1,
            it.author_handle.chars().take(16).collect::<String>(),
            it.metrics.like_count.unwrap_or(-1),
            it.metrics.share_count.unwrap_or(-1),
            it.metrics.comment_count.unwrap_or(-1),
            it.metrics.view_count.unwrap_or(-1),
            text
        );
    }
    Ok(())
}
