//! End-to-end smoke for YouTubeCrawler.
//!
//! Run with:
//!   PWRIGHT_BIN=/path/to/pwright PWRIGHT_CDP=http://host:9222 \
//!     cargo run --example smoke -p multipost-crawlers-youtube -- \
//!     https://www.youtube.com/@flipradio_fearnation/videos 20

use multipost_core::{CrawlOptions, Crawler};
use multipost_crawlers_youtube::YouTubeCrawler;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let source = args
        .next()
        .ok_or("usage: smoke <youtube-channel-or-videos-url> [duration_secs]")?;
    let duration_secs = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let opts = CrawlOptions {
        duration_secs,
        source_urls: vec![source],
        ..Default::default()
    };
    let items = YouTubeCrawler::new().run(&opts).await?;
    println!("captured {} video(s):", items.len());
    for (i, it) in items.iter().enumerate() {
        let title = it.body.chars().take(72).collect::<String>();
        println!(
            "  [{:>3}] views={:>8} | {:<18} | {}",
            i + 1,
            it.metrics.view_count.unwrap_or(-1),
            it.author_handle.chars().take(18).collect::<String>(),
            title
        );
    }
    Ok(())
}
