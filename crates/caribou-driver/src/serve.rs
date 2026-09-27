//! `caribou serve`: a project built for the browser and served to it, and
//! built again whenever one of its sources changes.
//!
//! The program is built for `wasm32-wasip1-threads` into the project's
//! `target/`, with Ash's page beside it, and `target/` is served over HTTP
//! with the headers a threaded module needs (COOP and COEP, which give the
//! page a `SharedArrayBuffer`). `localhost` is a secure origin, which WebGPU
//! and shared memory also need, so no certificate is involved; another
//! device reaching the LAN address has to be told to treat it as secure.
//!
//! The project file, its class paths and its plugin crates are watched.
//! After a build that succeeds, every open page reloads: the page is served
//! with a script listening on `/__caribou/events`. A build that fails is
//! reported here and the last good one stays served.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use crate::cbproj::Project;

/// The target a page runs: threads are Workers over shared memory.
pub const TARGET: &str = "wasm32-wasip1-threads";

/// Where a page hears that a new build is ready.
const EVENTS: &str = "/__caribou/events";

/// What the served page adds to Ash's: reload when a build is ready.
const RELOAD: &str =
    "<script>new EventSource(\"/__caribou/events\").onmessage = () => location.reload();</script>";

/// Build the project for the browser: the module in its `target/`, with
/// the page that runs it beside it (`aot::build_with_languages` writes it).
/// Returns the module.
pub fn build(project: &Project) -> Result<PathBuf> {
    let hl = project.compile_haxe()?;
    let target_dir = project.target_dir();
    let out = target_dir.join(format!("{}.wasm", project.name));
    let plugins: Vec<PathBuf> = project.plugins.values().cloned().collect();
    let module = crate::aot::build_with_languages(
        &hl,
        TARGET,
        Some(&out),
        &plugins,
        &project.side_modules,
        &project.sources,
        &project.languages,
        Some(&target_dir),
    )?;
    Ok(module)
}

/// Build the project and serve it on `port`, on every interface when
/// `lan`, else on localhost; rebuild on every change until interrupted.
pub fn serve(project: &Project, port: u16, lan: bool) -> Result<()> {
    let root = project.target_dir();
    std::fs::create_dir_all(&root)?;
    if let Err(e) = build(project) {
        eprintln!("caribou: {e:#}");
    }
    let host = if lan { "0.0.0.0" } else { "127.0.0.1" };
    let listener = TcpListener::bind((host, port))
        .with_context(|| format!("listening on port {port}; pass --port to pick another"))?;
    println!("serving {} at http://localhost:{port}/", project.name);
    if lan && let Some(address) = lan_address() {
        println!(
            "  and at http://{address}:{port}/ for other devices, whose browser must treat \
             that origin as secure (chrome://flags, \"Insecure origins treated as secure\")"
        );
    }
    let listeners: Arc<Mutex<Vec<TcpStream>>> = Arc::default();
    {
        let listeners = Arc::clone(&listeners);
        let root = root.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (root, listeners) = (root.clone(), Arc::clone(&listeners));
                std::thread::spawn(move || {
                    let _ = respond(stream, &root, &listeners);
                });
            }
        });
    }
    let mut seen = snapshot(project);
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let now = snapshot(project);
        if now == seen {
            continue;
        }
        println!("caribou: a source changed; building");
        match build(project) {
            Ok(_) => {
                println!("caribou: built; reloading the page");
                // A page whose stream is gone is dropped.
                listeners
                    .lock()
                    .unwrap()
                    .retain_mut(|page| page.write_all(b"data: reload\n\n").is_ok());
            }
            Err(e) => eprintln!("caribou: {e:#}"),
        }
        // What the build wrote is not a change.
        seen = snapshot(project);
    }
}

/// Every watched file's modification time and length: the project file,
/// its class paths, and its plugin crates, less anything under `target/`.
fn snapshot(project: &Project) -> BTreeMap<PathBuf, (SystemTime, u64)> {
    let target = project.target_dir();
    let mut files = BTreeMap::new();
    let roots = std::iter::once(project.path.clone())
        .chain(project.sources.iter().cloned())
        .chain(project.plugins.values().filter(|p| p.is_dir()).cloned());
    for root in roots {
        walk(&root, &target, &mut files);
    }
    files
}

fn walk(path: &Path, skip: &Path, files: &mut BTreeMap<PathBuf, (SystemTime, u64)>) {
    if path.starts_with(skip) || path.file_name().is_some_and(|n| n == "target") {
        return;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.is_dir() {
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            walk(&entry.path(), skip, files);
        }
    } else {
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        files.insert(path.to_path_buf(), (modified, meta.len()));
    }
}

/// Answer one request: a file under `root`, the page with its reload
/// script, or the stream of reloads.
fn respond(
    mut stream: TcpStream,
    root: &Path,
    listeners: &Mutex<Vec<TcpStream>>,
) -> std::io::Result<()> {
    let mut request = String::new();
    BufReader::new(&stream).read_line(&mut request)?;
    let target = request.split_whitespace().nth(1).unwrap_or("/");
    let path = target.split(['?', '#']).next().unwrap_or("/");
    if path == EVENTS {
        stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\
              Cross-Origin-Resource-Policy: same-origin\r\n\r\n",
        )?;
        listeners.lock().unwrap().push(stream);
        return Ok(());
    }
    let Some(file) = resolve(root, path) else {
        return send(&mut stream, "404 Not Found", "text/plain", b"not found");
    };
    let Ok(mut body) = std::fs::read(&file) else {
        return send(&mut stream, "404 Not Found", "text/plain", b"not found");
    };
    let kind = content_type(&file);
    if kind == "text/html" {
        body = with_reload(body);
    }
    send(&mut stream, "200 OK", kind, &body)
}

fn send(stream: &mut TcpStream, status: &str, kind: &str, body: &[u8]) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Cross-Origin-Opener-Policy: same-origin\r\nCross-Origin-Embedder-Policy: require-corp\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\nCache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

/// The file `path` names under `root`, `/` its `index.html`; none that
/// would leave it.
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let mut file = root.to_path_buf();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        let part = decode(part)?;
        if part == ".." || part == "." || part.contains(['/', '\\']) {
            return None;
        }
        file.push(part);
    }
    if file.is_dir() {
        file.push("index.html");
    }
    file.is_file().then_some(file)
}

/// `%xx` escapes decoded; `None` for a malformed one.
fn decode(part: &str) -> Option<String> {
    let bytes = part.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html",
        "js" | "mjs" => "text/javascript",
        "wasm" => "application/wasm",
        "json" => "application/json",
        "css" => "text/css",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

/// The page with the reload script before its `</body>`, or at its end.
fn with_reload(page: Vec<u8>) -> Vec<u8> {
    let text = String::from_utf8_lossy(&page);
    match text.rfind("</body>") {
        Some(at) => format!("{}{RELOAD}{}", &text[..at], &text[at..]).into_bytes(),
        None => format!("{text}{RELOAD}").into_bytes(),
    }
}

/// This machine's address on its local network, if it has one.
fn lan_address() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("10.255.255.255:1").ok()?;
    Some(socket.local_addr().ok()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_resolves_inside_the_served_directory_only() {
        let root = std::env::temp_dir().join(format!("caribou-serve-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub dir")).unwrap();
        std::fs::write(root.join("index.html"), "i").unwrap();
        std::fs::write(root.join("sub dir/a.wasm"), "a").unwrap();
        assert_eq!(resolve(&root, "/"), Some(root.join("index.html")));
        assert_eq!(
            resolve(&root, "/sub%20dir/a.wasm"),
            Some(root.join("sub dir/a.wasm"))
        );
        assert_eq!(resolve(&root, "/../etc/passwd"), None);
        assert_eq!(resolve(&root, "/sub%20dir/..%2F..%2Fx"), None);
        assert_eq!(resolve(&root, "/missing.js"), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_page_is_served_with_its_reload_script() {
        let page = with_reload(b"<html><body>x</body></html>".to_vec());
        let page = String::from_utf8(page).unwrap();
        assert!(page.contains(&format!("x{RELOAD}</body>")));
        assert!(
            String::from_utf8(with_reload(b"bare".to_vec()))
                .unwrap()
                .ends_with(RELOAD)
        );
    }
}
