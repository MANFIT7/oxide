//! A tiny reverse proxy that fronts a localhost dev server and injects an
//! element-picker script into HTML responses. Because the previewed page is
//! then served from the proxy's own origin, the injected script can
//! `postMessage` selected-element data up to the Oxide webview — giving a
//! Cursor-style "select element → send to chat" flow without CDP.
//!
//! One proxy listener is started lazily; its upstream target port is swappable
//! at runtime (`set_target`) so switching previewed servers is instant.

use std::sync::atomic::{AtomicU16, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

static TARGET: AtomicU16 = AtomicU16::new(0);
static PROXY_PORT: AtomicU16 = AtomicU16::new(0);

/// Point the proxy at a new upstream localhost port.
pub fn set_target(port: u16) {
    TARGET.store(port, Ordering::Relaxed);
}

/// Start the proxy once and return its port (0 on bind failure). Idempotent.
pub async fn ensure_proxy() -> u16 {
    let existing = PROXY_PORT.load(Ordering::Relaxed);
    if existing != 0 {
        return existing;
    }
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(_) => return 0,
    };
    let port = match listener.local_addr() {
        Ok(a) => a.port(),
        Err(_) => return 0,
    };
    // Claim the slot; if another task won the race, drop our listener.
    if PROXY_PORT
        .compare_exchange(0, port, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return PROXY_PORT.load(Ordering::Relaxed);
    }
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        loop {
            if let Ok((sock, _)) = listener.accept().await {
                let client = client.clone();
                tokio::spawn(async move {
                    let _ = handle(sock, client).await;
                });
            }
        }
    });
    port
}

async fn handle(mut sock: tokio::net::TcpStream, client: reqwest::Client) -> std::io::Result<()> {
    // Read request headers (up to the blank line).
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 2048];
    loop {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 64 * 1024 {
            break;
        }
    }
    let head_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .unwrap_or(buf.len());
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let req_line = lines.next().unwrap_or("");
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut content_len = 0usize;
    for l in lines {
        if let Some(v) = l.to_ascii_lowercase().strip_prefix("content-length:") {
            content_len = v.trim().parse().unwrap_or(0);
        }
    }
    // Read any remaining body bytes.
    let mut body = buf[head_end..].to_vec();
    while body.len() < content_len {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }

    let target = TARGET.load(Ordering::Relaxed);
    if target == 0 {
        let _ = sock.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        return Ok(());
    }
    let url = format!("http://127.0.0.1:{target}{path}");
    let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut rb = client.request(m, &url).header("Accept-Encoding", "identity");
    if !body.is_empty() {
        rb = rb.body(body);
    }
    let resp = match rb.send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Oxide preview proxy: upstream localhost:{target} unreachable — {e}");
            let out = format!(
                "HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                msg.len(), msg
            );
            let _ = sock.write_all(out.as_bytes()).await;
            return Ok(());
        }
    };
    let status = resp.status();
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let bytes = resp.bytes().await.unwrap_or_default();

    let out_body: Vec<u8> = if ct.contains("text/html") {
        let html = String::from_utf8_lossy(&bytes);
        let inject = format!("<script>{PICKER_JS}</script>");
        let injected = if let Some(i) = html.rfind("</body>") {
            format!("{}{}{}", &html[..i], inject, &html[i..])
        } else {
            format!("{html}{inject}")
        };
        injected.into_bytes()
    } else {
        bytes.to_vec()
    };

    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("OK"),
        ct,
        out_body.len()
    );
    sock.write_all(header.as_bytes()).await?;
    sock.write_all(&out_body).await?;
    sock.flush().await?;
    Ok(())
}

/// Element-picker injected into previewed pages. Toggled by `postMessage`
/// from the parent (`oxide-pick-on`/`oxide-pick-off`); on click it posts the
/// selected element's selector, outerHTML, React component + source file:line.
const PICKER_JS: &str = r#"
(function(){
  if (window.__oxidePick) return; window.__oxidePick = true;
  var on=false, hl=null, last=null;
  function overlay(){ if(hl) return hl; hl=document.createElement('div');
    hl.style.cssText='position:fixed;z-index:2147483647;pointer-events:none;border:2px solid #6073cc;background:rgba(96,115,204,.15);border-radius:3px;display:none';
    (document.body||document.documentElement).appendChild(hl); return hl; }
  function rectTo(el){ var r=el.getBoundingClientRect(); var o=overlay();
    o.style.display='block'; o.style.left=r.left+'px'; o.style.top=r.top+'px'; o.style.width=r.width+'px'; o.style.height=r.height+'px'; }
  function selector(el){ var p=[]; while(el&&el.nodeType===1&&p.length<5){ if(el.id){ p.unshift('#'+el.id); break; } var s=el.tagName.toLowerCase(); if(el.className&&typeof el.className==='string'){ var c=el.className.trim().split(/\s+/).slice(0,2).join('.'); if(c) s+='.'+c; } var par=el.parentNode; if(par){ var sib=[].slice.call(par.children).filter(function(x){return x.tagName===el.tagName;}); if(sib.length>1) s+=':nth-of-type('+(sib.indexOf(el)+1)+')'; } p.unshift(s); el=el.parentNode; } return p.join(' > '); }
  function fiber(el){ for(var k in el){ if(k.indexOf('__reactFiber$')===0||k.indexOf('__reactInternalInstance$')===0) return el[k]; } return null; }
  function reactSource(el){ var f=fiber(el), g=0; while(f&&g++<40){ if(f._debugSource){ return f._debugSource.fileName+':'+f._debugSource.lineNumber; } f=f.return; } return null; }
  function reactName(el){ var f=fiber(el), g=0; while(f&&g++<40){ var t=f.type; if(t&&(t.displayName||t.name)) return t.displayName||t.name; f=f.return; } return null; }
  function onMove(e){ if(!on) return; var el=document.elementFromPoint(e.clientX,e.clientY); if(el&&el!==hl){ last=el; rectTo(el); } }
  function onClick(e){ if(!on) return; e.preventDefault(); e.stopPropagation();
    var el=last||e.target;
    var info={ type:'oxide-element', tag:el.tagName.toLowerCase(), selector:selector(el),
      text:(el.innerText||'').replace(/\s+/g,' ').trim().slice(0,120),
      html:el.outerHTML.slice(0,700), component:reactName(el), source:reactSource(el) };
    window.parent.postMessage(info,'*'); setOn(false);
  }
  function setOn(v){ on=v; overlay().style.display='none'; document.documentElement.style.cursor=v?'crosshair':''; }
  window.addEventListener('message',function(e){ if(e.data==='oxide-pick-on') setOn(true); else if(e.data==='oxide-pick-off') setOn(false); });
  document.addEventListener('mousemove',onMove,true);
  document.addEventListener('click',onClick,true);
})();
"#;
