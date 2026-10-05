use std::{env, fs, path::PathBuf};
fn main() {
    if env::var_os("CARGO_FEATURE_CONTROLLER").is_none() {
        return;
    }
    let dist = PathBuf::from("../web/dist-live");
    println!("cargo:rerun-if-changed={}", dist.display());
    let html = fs::canonicalize(dist.join("live.html"))
        .expect("build web UI first: cd source/web && npm ci && npm run build");
    let mut code = format!(
        "static WEB_HTML:&[u8]=include_bytes!({:?});\nstatic WEB_ASSETS:&[(&str,&str,&[u8])]=&[\n",
        html
    );
    let assets = fs::read_dir(dist.join("assets")).expect("web assets");
    for file in assets {
        let path = file.unwrap().path();
        if !path.is_file() {
            continue;
        }
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        let mime = match ext {
            "js" => "text/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "woff2" => "font/woff2",
            _ => continue,
        };
        code.push_str(&format!(
            "({:?},{:?},include_bytes!({:?})),\n",
            format!("/assets/{}", path.file_name().unwrap().to_string_lossy()),
            mime,
            fs::canonicalize(path).unwrap()
        ));
    }
    code.push_str("];\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("web_assets.rs"),
        code,
    )
    .unwrap();
}
