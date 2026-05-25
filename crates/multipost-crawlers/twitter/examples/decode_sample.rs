//! Decode a saved HomeTimeline JSON file and print one line per tweet.

use chrono::Utc;
use multipost_crawlers_twitter::decode_home_timeline;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: decode_sample <json>")?;
    let raw = std::fs::read_to_string(&path)?;
    let items = decode_home_timeline(&raw, Utc::now())?;
    println!("decoded {} item(s):", items.len());
    for (i, it) in items.iter().enumerate() {
        let text = it.body.chars().take(60).collect::<String>().replace('\n', " ");
        println!(
            "  [{:>2}] @{:<18} | rt={:>5} fav={:>5} rep={:>4} bm={:>4} views={:>7} | {}",
            i + 1,
            it.author_handle.chars().take(18).collect::<String>(),
            it.metrics.share_count.unwrap_or(-1),
            it.metrics.like_count.unwrap_or(-1),
            it.metrics.comment_count.unwrap_or(-1),
            it.metrics.bookmark_count.unwrap_or(-1),
            it.metrics.view_count.unwrap_or(-1),
            text
        );
    }
    Ok(())
}
