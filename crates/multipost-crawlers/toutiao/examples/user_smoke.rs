//! End-to-end smoke for ToutiaoUserCrawler against a live Chrome.
//!
//! Usage:
//!   PWRIGHT_CDP=http://alienware-win-yuacx.tail8f3b66.ts.net:9222 \
//!   PWRIGHT_BIN=/path/to/pwright \
//!   cargo run -p multipost-crawlers-toutiao --example user_smoke -- <user_token> 30
use multipost_core::{UserCrawlOptions, UserCrawler};
use multipost_crawlers_toutiao::ToutiaoUserCrawler;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let token = std::env::args()
        .nth(1)
        .expect("usage: user_smoke <user_token> [max]");
    let max: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let opts = UserCrawlOptions {
        handle: token.clone(),
        max_posts: max,
        max_duration_secs: 90,
        ..Default::default()
    };
    println!("crawling token {token} (max {max}) via {:?}", opts.cdp_url);
    let items = ToutiaoUserCrawler::new().crawl_user(&opts).await?;
    println!("captured {} post(s):", items.len());
    for (i, it) in items.iter().enumerate() {
        let text = it
            .body
            .chars()
            .take(50)
            .collect::<String>()
            .replace('\n', " ");
        println!(
            "  [{:>3}] {:<10} read={:>6} like={:>5} cmt={:>4} bm={:>4} | {}",
            i + 1,
            it.author_handle.chars().take(10).collect::<String>(),
            it.metrics.read_count.unwrap_or(-1),
            it.metrics.like_count.unwrap_or(-1),
            it.metrics.comment_count.unwrap_or(-1),
            it.metrics.bookmark_count.unwrap_or(-1),
            text
        );
    }
    Ok(())
}
