//! Smoke-run the Toutiao stats collector against a live logged-in Chrome.
//!
//! Usage: `CDP_URL=http://localhost:9333 cargo run -p \
//!   multipost-publishers-toutiao --example stats_smoke`

use multipost_core::{StatsCollector, StatsOptions};
use multipost_publishers_toutiao::ToutiaoStatsCollector;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cdp = std::env::var("CDP_URL").expect("set CDP_URL");
    let max_posts: usize = std::env::var("MAX_POSTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    let opts = StatsOptions {
        cdp_url: Some(cdp),
        max_posts,
        handle: None,
        pwright_bin: "pwright".into(),
    };
    let snap = ToutiaoStatsCollector::new().collect(&opts).await?;

    println!("\n=== ACCOUNT ===");
    println!("{:#?}", snap.account);
    println!("\n=== POSTS ({}) ===", snap.posts.len());
    println!(
        "  {:>3}  {:<7} {:>6} {:>6} {:>5} {:>4} {:>4}  title",
        "#", "type", "impr", "read", "like", "cmt", "fav"
    );
    let fi = |o: Option<i64>| o.map(|v| v.to_string()).unwrap_or_else(|| "—".into());
    for (i, p) in snap.posts.iter().enumerate() {
        println!(
            "  {:>3}  {:<7} {:>6} {:>6} {:>5} {:>4} {:>4}  {}",
            i + 1,
            p.post_type,
            fi(p.impressions),
            fi(p.reads),
            fi(p.likes),
            fi(p.comments),
            fi(p.bookmarks),
            p.title.chars().take(34).collect::<String>(),
        );
    }
    Ok(())
}
