//! Minimal CDP byte-injection probe.
//!
//! Isolates multipost's `cdp.rs` (BrowserSession / PageSession /
//! upload_files_to_input) from the full 微头条 publish flow: connect to the
//! Chrome, open the editor, click 图片, byte-inject one image, then poll for
//! the drawer to register the upload ("已上传 N" + 确定 enabled).
//!
//! If this REGISTERS, multipost's CDP layer is fine and the full-flow bug is
//! elsewhere (body-fill / sequencing). If it FAILS where a standalone Python
//! CDP inject succeeds, the bug is in this Rust CDP layer / WS transport.
//!
//! Usage:
//!   CDP_URL=http://alienware-win-yuacx:9402 \
//!   IMG=/home/cyuan/.multipost/media/<id>.jpg \
//!   FILL_BODY=1 \                # optional: fill the ProseMirror body first
//!   cargo run -p multipost-publishers-toutiao --example inject_probe

use multipost_publishers_toutiao::cdp::BrowserSession;
use multipost_publishers_toutiao::selectors;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cdp = std::env::var("CDP_URL").unwrap_or_else(|_| "http://alienware-win-yuacx:9402".into());
    let img = std::env::var("IMG").unwrap_or_else(|_| {
        "/home/cyuan/.multipost/media/b4a57e4b-52a9-406e-8351-c72036bd13cc.jpg".into()
    });
    let fill_body = std::env::var("FILL_BODY").is_ok();
    let bytes = std::fs::read(&img)?;
    println!(
        "CDP={cdp}\nIMG={img} bytes={} fill_body={fill_body}",
        bytes.len()
    );

    let session = BrowserSession::connect(&cdp).await?;

    // Optionally mirror the server's pre-publish check_auth: it creates a probe
    // tab on the SAME Chrome, navigates to /profile_v4, polls, then closes it,
    // right before the publish opens its editor tab. CHECK_AUTH=1 to replicate.
    if std::env::var("CHECK_AUTH").is_ok() {
        let probe_tab = session
            .create_tab("https://mp.toutiao.com/profile_v4/index")
            .await?;
        println!("check_auth-style probe tab opened: {}", probe_tab.id);
        tokio::time::sleep(Duration::from_secs(4)).await;
        let _ = session.close_tab(&probe_tab.id).await;
        println!("check_auth-style probe tab closed");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let tab = session.create_tab(selectors::WEITOUTIAO_URL).await?;
    let mut page = session.open_page(&tab).await?;
    page.bring_to_front().await?;
    println!("tab={} (foregrounded)", tab.id);

    // wait for the editor to mount
    let t = Instant::now();
    loop {
        let ok = page
            .evaluate(
                r#"(() => !!document.querySelector('.ProseMirror[contenteditable="true"]'))()"#,
            )
            .await?
            .as_bool()
            .unwrap_or(false);
        if ok {
            break;
        }
        if t.elapsed() > Duration::from_secs(45) {
            anyhow::bail!("editor mount timeout");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    println!("editor mounted");

    // DRAFT STATE on open (before any clearing): does a prior failed attempt's
    // draft auto-load (text and/or images)?
    let draft = page
        .evaluate(
            r#"(() => { const ed = document.querySelector('.ProseMirror[contenteditable="true"]');
                const txt = ed ? (ed.innerText || '').trim() : '';
                const imgs = ed ? ed.querySelectorAll('img').length : -1;
                const any_img = document.querySelectorAll('.ProseMirror img').length;
                return JSON.stringify({ editor_text_len: txt.length, editor_text_head: txt.slice(0, 30), editor_imgs: imgs, any_editor_imgs: any_img }); })()"#,
        )
        .await?;
    println!("DRAFT-ON-OPEN: {}", draft.as_str().unwrap_or("?"));

    if fill_body {
        // Faithfully mirror multipost: clear draft, then chunked execCommand
        // insertText at 800 chars with 50ms settle, with a LONG real-length body.
        let body: String = "测试正文用于复现配图发布流程的文字内容，".repeat(60); // ~1200 chars
        page.evaluate(
            r#"(() => { const ed = document.querySelector('.ProseMirror[contenteditable="true"]');
                ed.focus(); document.execCommand('selectAll', false, null);
                document.execCommand('delete', false, null); return true; })()"#,
        )
        .await?;
        let chars: Vec<char> = body.chars().collect();
        for chunk in chars.chunks(800) {
            let piece: String = chunk.iter().collect();
            let js = format!(
                r#"(() => {{ document.execCommand('insertText', false, {}); return true; }})()"#,
                serde_json::to_string(&piece)?
            );
            page.evaluate(&js).await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        println!("body filled ({} chars)", body.chars().count());
    }

    // click 图片, wait for the file input to mount
    page.evaluate(
        r#"(() => { const b = [...document.querySelectorAll('button')]
            .find(x => (x.innerText || '').trim() === '图片' && x.offsetParent !== null);
            if (b) b.click(); return !!b; })()"#,
    )
    .await?;
    let t = Instant::now();
    loop {
        if page
            .evaluate(
                r#"(() => !!document.querySelector('input[type="file"][accept*="image"]'))()"#,
            )
            .await?
            .as_bool()
            .unwrap_or(false)
        {
            break;
        }
        if t.elapsed() > Duration::from_secs(20) {
            anyhow::bail!("file input mount timeout");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    println!("clicked 图片, file input present");

    // THE inject -- multipost's own cdp.rs byte-injection
    let files: Vec<(&str, &str, &[u8])> = vec![("p.jpg", "image/jpeg", bytes.as_slice())];
    page.upload_files_to_input(selectors::WEITOUTIAO_IMAGE_INPUT, &files)
        .await?;
    println!("upload_files_to_input returned ok");

    // poll the drawer for a REAL upload (已上传 + 确定 enabled)
    let t = Instant::now();
    loop {
        let v = page
            .evaluate(
                r#"(() => { const m = (document.body.innerText || '').match(/已上传\s*(\d+)\s*张图片/);
                    const b = document.querySelector('button[data-e2e="imageUploadConfirm-btn"]');
                    return JSON.stringify({ uploaded: m ? m[1] : null, confirm: !!b && !b.disabled }); })()"#,
            )
            .await?;
        let last = v.as_str().unwrap_or("").to_string();
        if last.contains("\"uploaded\":\"1\"") && last.contains("\"confirm\":true") {
            println!(
                "UPLOAD REGISTERED after {:.1}s -> {last}",
                t.elapsed().as_secs_f64()
            );
            break;
        }
        if t.elapsed() > Duration::from_secs(25) {
            println!("RESULT: NOT REGISTERED after 25s -> {last}");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let vp = page
        .evaluate(r#"JSON.stringify({w: window.innerWidth, h: window.innerHeight})"#)
        .await?;
    println!("viewport = {}", vp.as_str().unwrap_or("?"));

    // ---- TRUSTED real_click on 确定, then a FULL post-click dump ----
    let rect = page
        .evaluate(
            r#"(() => { const b = document.querySelector('button[data-e2e="imageUploadConfirm-btn"]');
                if (!b) return "null"; const r = b.getClientRects()[0]; if (!r) return "null";
                return JSON.stringify({ x: r.x + r.width / 2, y: r.y + r.height / 2 }); })()"#,
        )
        .await?;
    let rect = rect.as_str().unwrap_or("null").to_string();
    println!("confirm rect = {rect}");
    let v: serde_json::Value = serde_json::from_str(&rect).unwrap_or(serde_json::Value::Null);
    if let (Some(x), Some(y)) = (
        v.get("x").and_then(|x| x.as_f64()),
        v.get("y").and_then(|x| x.as_f64()),
    ) {
        page.real_click(x, y).await?;
        println!("trusted real_click at ({x:.0},{y:.0})");
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let dump = page
            .evaluate(
                r#"(() => {
                    const drawer = document.querySelector('.byte-drawer-wrapper');
                    const cb = document.querySelector('button[data-e2e="imageUploadConfirm-btn"]');
                    const body = document.body.innerText || '';
                    const toast = (body.match(/请上传图片|上传失败|失败|不支持|超过|错误|请稍后/) || [''])[0];
                    return JSON.stringify({
                        drawer_open: !!drawer,
                        drawer_text: drawer ? (drawer.innerText || '').replace(/\n/g, ' ').slice(0, 120) : null,
                        confirm_present: !!cb,
                        confirm_disabled: cb ? cb.disabled : null,
                        toast,
                    });
                })()"#,
            )
            .await?;
        println!("POST-CLICK DUMP: {}", dump.as_str().unwrap_or("?"));
    }

    // Did the image actually get inserted into the editor?
    let img_in_editor = page
        .evaluate(
            r#"(() => { const CDN = /p\d+-sign|sf\d+-cdn-tos|image-tt|toutiaoimg|pgc-image/;
                return [...document.querySelectorAll('.ProseMirror img, [class*=editor] img')]
                    .filter(im => CDN.test(im.src || '') && (im.naturalWidth || 0) > 40).length; })()"#,
        )
        .await?;
    println!("RESULT: imgs_in_editor={img_in_editor}");
    Ok(())
}
