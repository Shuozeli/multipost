//! Probe Toutiao's video upload editor without uploading.
//!
//! Usage:
//!   CDP_URL=http://alienware-win-yuacx:9222 \
//!   cargo run -p multipost-publishers-toutiao --example video_probe
//!
//! Optional:
//!   REMOTE_VIDEO_PATH=C:/Users/cyuan/Videos/multipost-uploads/probe.mp4 \
//!   cargo run -p multipost-publishers-toutiao --example video_probe

use multipost_publishers_toutiao::cdp::BrowserSession;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cdp = std::env::var("CDP_URL").unwrap_or_else(|_| "http://alienware-win-yuacx:9222".into());
    let url = std::env::var("TOUTIAO_VIDEO_URL")
        .unwrap_or_else(|_| "https://mp.toutiao.com/profile_v4/upload-video".into());
    println!("CDP={cdp}");
    println!("URL={url}");

    let session = BrowserSession::connect(&cdp).await?;
    let tab = if std::env::var("ATTACH_EXISTING").is_ok() {
        let pages: Vec<_> = session
            .list_pages()
            .await?
            .into_iter()
            .filter(|p| p.url.contains("/profile_v4/xigua/upload-video"))
            .collect();
        let mut chosen = None;
        for candidate in &pages {
            let mut p = session.open_page(candidate).await?;
            let body = p
                .evaluate("document.body ? document.body.innerText : ''")
                .await?
                .as_str()
                .unwrap_or("")
                .to_string();
            if body.contains("万亿IPO抽水") || body.contains("上传成功") {
                chosen = Some(candidate.clone());
                break;
            }
        }
        chosen.unwrap_or_else(|| pages.into_iter().next().unwrap())
    } else {
        session.create_tab(&url).await?
    };
    let mut page = session.open_page(&tab).await?;
    page.bring_to_front().await?;
    tokio::time::sleep(Duration::from_secs(8)).await;

    if let Ok(remote) = std::env::var("REMOTE_VIDEO_PATH") {
        let doc = page.get_document().await?;
        let node = page.query_selector(doc, "input[type=file]").await?;
        if node == 0 {
            anyhow::bail!("no input[type=file] on video upload page");
        }
        page.set_file_input_files(node, &[remote]).await?;
        println!("set_file_input_files ok; waiting for editor form ...");
        tokio::time::sleep(Duration::from_secs(25)).await;
    }

    let dump = page
        .evaluate(
            r#"(() => JSON.stringify({
                  href: location.href,
                  viewport: {w: window.innerWidth, h: window.innerHeight, sx: scrollX, sy: scrollY},
                  title: document.title,
              body_head: (document.body.innerText || '').replace(/\s+/g, ' ').slice(0, 1600),
              file_inputs: [...document.querySelectorAll('input[type=file]')].map((i, n) => ({
                n,
                accept: i.accept || '',
                multiple: !!i.multiple,
                visible: !!(i.offsetWidth || i.offsetHeight || i.getClientRects().length),
                cls: i.className || '',
              })),
              text_inputs: [...document.querySelectorAll('input, textarea, [contenteditable=true]')].slice(0, 40).map((i, n) => ({
                n,
                tag: i.tagName,
                type: i.getAttribute('type') || '',
                name: i.getAttribute('name') || '',
                value: i.value || '',
                checked: !!i.checked,
                placeholder: i.getAttribute('placeholder') || '',
                aria: i.getAttribute('aria-label') || '',
                text: (i.innerText || i.value || '').slice(0, 80),
                visible: !!(i.offsetWidth || i.offsetHeight || i.getClientRects().length),
              })),
              buttons: [...document.querySelectorAll('button, [role=button], a')].slice(0, 120).map((b, n) => ({
                n,
                tag: b.tagName,
                text: (b.innerText || '').trim().slice(0, 80),
                aria: b.getAttribute('aria-label') || '',
                disabled: !!b.disabled || b.getAttribute('aria-disabled') === 'true',
                href: b.href || b.getAttribute('href') || '',
                visible: !!(b.offsetWidth || b.offsetHeight || b.getClientRects().length),
              })).filter(b => b.visible && (b.text || b.aria || b.href)),
              publish_nodes: [...document.querySelectorAll('*')]
                .filter(e => (e.innerText || '').trim() === '发布')
                .slice(0, 20)
                .map((e, n) => {
                  const r = e.getBoundingClientRect();
                  return {
                    n,
                    tag: e.tagName,
                    cls: String(e.className || '').slice(0, 160),
                    disabled: !!e.disabled || e.getAttribute('aria-disabled') === 'true',
                    visible: !!(r.width || r.height || e.getClientRects().length),
                    rect: {x: r.x, y: r.y, w: r.width, h: r.height},
                    html: e.outerHTML.slice(0, 400),
                  };
                }),
            }))()"#,
        )
        .await?;
    println!("{}", dump.as_str().unwrap_or("{}"));

    if std::env::var("CLICK_PUBLISH").is_ok() {
        if let Ok(cover) = std::env::var("REMOTE_THUMBNAIL_PATH") {
            let clicked = page
                .evaluate(
                    r#"(() => {
                      const el = [...document.querySelectorAll('button, [role=button], span, div')]
                        .find(e => (e.innerText || '').trim() === '上传封面' && e.offsetParent !== null);
                      if (!el) return false;
                      el.scrollIntoView({block:'center', inline:'center'});
                      el.click();
                      return true;
                    })()"#,
                )
                .await?;
            println!("clicked cover upload = {}", clicked);
            tokio::time::sleep(Duration::from_secs(2)).await;
            let doc = page.get_document().await?;
            let node = page
                .query_selector(doc, "input[type=file][accept*='image']")
                .await?;
            println!("cover input node = {node}");
            if node != 0 {
                page.set_file_input_files(node, &[cover]).await?;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
        let tip = page
            .evaluate(
                r#"(() => {
                  const el = [...document.querySelectorAll('button, [role=button], span, div')]
                    .find(e => ['我知道了', '知道了', '关闭'].includes((e.innerText || '').trim()) && e.offsetParent !== null);
                  if (!el) return 'null';
                  el.scrollIntoView({block: 'center', inline: 'center'});
                  const r = el.getBoundingClientRect();
                  return JSON.stringify({x: r.x + r.width / 2, y: r.y + r.height / 2, text: (el.innerText || '').trim()});
                })()"#,
            )
            .await?;
        if tip.as_str().unwrap_or("null") != "null" {
            println!("tip rect = {}", tip.as_str().unwrap_or("null"));
            let v: serde_json::Value = serde_json::from_str(tip.as_str().unwrap_or("null"))?;
            let x = v.get("x").and_then(|v| v.as_f64()).unwrap();
            let y = v.get("y").and_then(|v| v.as_f64()).unwrap();
            page.real_click(x, y).await?;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let decl = page
            .evaluate(
                r#"(() => {
                  const el = [...document.querySelectorAll('button, [role=button], label, span, div')]
                    .find(e => (e.innerText || '').trim() === '投资观点，仅供参考' && e.offsetParent !== null);
                  if (!el) return 'null';
                  el.scrollIntoView({block: 'center', inline: 'center'});
                  const r = el.getBoundingClientRect();
                  return JSON.stringify({x: r.x + r.width / 2, y: r.y + r.height / 2, text: (el.innerText || '').trim()});
                })()"#,
            )
            .await?;
        if decl.as_str().unwrap_or("null") != "null" {
            println!("declaration rect = {}", decl.as_str().unwrap_or("null"));
            let v: serde_json::Value = serde_json::from_str(decl.as_str().unwrap_or("null"))?;
            let x = v.get("x").and_then(|v| v.as_f64()).unwrap();
            let y = v.get("y").and_then(|v| v.as_f64()).unwrap();
            page.real_click(x, y).await?;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        page.evaluate(
            r#"(() => {
              const text = '美股公司新闻与流动性观察。';
              const el = [...document.querySelectorAll('textarea')]
                .find(t => (t.placeholder || '').includes('视频简介')) || document.querySelector('textarea');
              if (!el) return false;
              const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
              setter.call(el, text);
              el.dispatchEvent(new InputEvent('input', {bubbles:true, inputType:'insertText', data:text}));
              el.dispatchEvent(new Event('blur', {bubbles:true}));
              el.dispatchEvent(new Event('change', {bubbles:true}));
              return true;
            })()"#,
        )
        .await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let rect = page
            .evaluate(
                r#"(() => {
                  const buttons = [...document.querySelectorAll('button')].filter(b => b.offsetParent !== null);
                  const b = buttons.find(x => (x.innerText || '').trim() === '发布');
                  if (!b) return 'null';
                  b.scrollIntoView({block: 'center', inline: 'center'});
                  const r = b.getBoundingClientRect();
                  return JSON.stringify({x: r.x + r.width / 2, y: r.y + r.height / 2, disabled: !!b.disabled || b.getAttribute('aria-disabled') === 'true'});
                })()"#,
            )
            .await?;
        println!("publish rect = {}", rect.as_str().unwrap_or("null"));
        if rect.as_str().unwrap_or("null") == "null" {
            return Ok(());
        }
        let v: serde_json::Value = serde_json::from_str(rect.as_str().unwrap_or("null"))?;
        let x = v.get("x").and_then(|v| v.as_f64()).unwrap();
        let y = v.get("y").and_then(|v| v.as_f64()).unwrap();
        page.real_click(x, y).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        page.real_click(x, y).await?;
        page.evaluate(
            r#"(() => {
              const b = document.querySelector('button.submit') ||
                [...document.querySelectorAll('button')].find(x => (x.innerText || '').trim() === '发布');
              if (!b) return false;
              for (const type of ['pointerover','mouseover','pointerdown','mousedown','pointerup','mouseup','click']) {
                b.dispatchEvent(new MouseEvent(type, {bubbles:true, cancelable:true, view:window, buttons: type.includes('down') ? 1 : 0}));
              }
              return true;
            })()"#,
        ).await?;
        if std::env::var("CALL_REACT_CLICK").is_ok() {
            let called = page
                .evaluate(
                    r#"(() => {
                      const b = document.querySelector('button.submit') ||
                        [...document.querySelectorAll('button')].find(x => (x.innerText || '').trim() === '发布');
                      if (!b) return 'no button';
                      const keys = Object.keys(b);
                      const propsKey = keys.find(k => k.startsWith('__reactProps$'));
                      const handlersKey = keys.find(k => k.startsWith('__reactEventHandlers$'));
                      const fiberKey = keys.find(k => k.startsWith('__reactFiber$'));
                      const out = {keys, propsKey, handlersKey, fiberKey, called:false};
                      const props = propsKey ? b[propsKey] : (handlersKey ? b[handlersKey] : null);
                      if (props && typeof props.onClick === 'function') {
                        props.onClick({type:'click', target:b, currentTarget:b, preventDefault(){}, stopPropagation(){}, nativeEvent:{}});
                        out.called = true;
                      }
                      return JSON.stringify(out);
                    })()"#,
                )
                .await?;
            println!("react click = {}", called.as_str().unwrap_or("?"));
        }
        println!("real_click publish at ({x:.0},{y:.0})");
        tokio::time::sleep(Duration::from_secs(20)).await;
        let after = page
            .evaluate(
                r#"(() => JSON.stringify({
                  href: location.href,
                  text: (document.body.innerText || '').replace(/\s+/g, ' ').slice(0, 1200)
                }))()"#,
            )
            .await?;
        println!("AFTER_CLICK {}", after.as_str().unwrap_or("{}"));
    }
    Ok(())
}
