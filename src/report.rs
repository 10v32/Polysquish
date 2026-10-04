//! Standalone HTML report written next to the exported files.

use crate::pipeline::SquishResult;
use anyhow::Result;
use std::fmt::Write as _;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn fmt_int(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn fmt_bytes(b: u64) -> String {
    let f = b as f64;
    if f > 1e9 {
        format!("{:.2} GB", f / 1e9)
    } else if f > 1e6 {
        format!("{:.1} MB", f / 1e6)
    } else if f > 1e3 {
        format!("{:.0} KB", f / 1e3)
    } else {
        format!("{b} B")
    }
}

pub fn render(result: &SquishResult, recipe_json: &str) -> Result<String> {
    let mut h = String::new();
    let before = &result.before;
    let after = &result.after;
    let reduction = if before.triangles > 0 {
        100.0 * (1.0 - after.triangles as f64 / before.triangles as f64)
    } else {
        0.0
    };
    write!(h, r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Polysquish report · {name}</title>
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>
:root{{--bg:#0b0716;--card:rgba(255,255,255,.04);--border:rgba(255,255,255,.08);--text:#efe9ff;--muted:#a99cd1;--pink:#ff6ad5;--violet:#9b7bff;--mint:#5ff2c3;--amber:#ffc466}}
*{{box-sizing:border-box}}body{{margin:0;font-family:Inter,"SF Pro Display","Segoe UI",Roboto,system-ui,sans-serif;background:radial-gradient(1200px 600px at 10% -10%,rgba(255,79,216,.18),transparent 60%),radial-gradient(900px 500px at 100% 0,rgba(124,92,255,.22),transparent 60%),linear-gradient(180deg,#0b0716,#150b2e);color:var(--text);min-height:100vh;padding:40px 20px}}
main{{max-width:1040px;margin:0 auto}}h1{{font-weight:700;letter-spacing:-.02em;margin:0 0 4px;font-size:32px}}h1 span{{background:linear-gradient(135deg,#ff5fd2,#8b5cf6);-webkit-background-clip:text;background-clip:text;color:transparent}}
.sub{{color:var(--muted);margin:0 0 28px}}.grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:16px;margin-bottom:20px}}
.card{{background:var(--card);border:1px solid var(--border);border-radius:20px;padding:20px;backdrop-filter:blur(10px)}}.card h2{{font-size:12px;letter-spacing:.14em;text-transform:uppercase;color:var(--muted);margin:0 0 10px;font-weight:600}}
.big{{font-size:30px;font-weight:700;letter-spacing:-.02em}}.delta{{display:inline-block;margin-left:8px;padding:3px 10px;border-radius:999px;font-size:13px;background:rgba(95,242,195,.12);color:var(--mint);border:1px solid rgba(95,242,195,.3)}}
table{{width:100%;border-collapse:collapse;font-size:14px}}td,th{{padding:8px 6px;text-align:left;border-bottom:1px solid var(--border)}}th{{color:var(--muted);font-weight:500}}
ul{{margin:0;padding-left:18px;line-height:1.7}}.tex{{display:grid;grid-template-columns:repeat(auto-fill,minmax(160px,1fr));gap:12px}}.tex img{{width:100%;border-radius:12px;border:1px solid var(--border);image-rendering:auto}}.tex figcaption{{font-size:12px;color:var(--muted);margin-top:6px}}
pre{{background:rgba(0,0,0,.3);border:1px solid var(--border);border-radius:12px;padding:14px;overflow:auto;font-size:12px;color:#d9d0ff}}code{{color:var(--pink)}}footer{{color:var(--muted);font-size:12px;margin-top:32px;text-align:center}}
</style></head><body><main>
<h1><span>Polysquish</span> report</h1><p class="sub">{name} · {fmt} source · {date}</p>
<div class="grid">
<div class="card"><h2>Triangles</h2><div class="big">{after_tri}<span class="delta">−{red:.1}%</span></div><div style="color:var(--muted)">from {before_tri}</div></div>
<div class="card"><h2>Vertices</h2><div class="big">{after_v}</div><div style="color:var(--muted)">from {before_v}</div></div>
<div class="card"><h2>File size</h2><div class="big">{after_b}</div><div style="color:var(--muted)">from {before_b}</div></div>
<div class="card"><h2>Textures</h2><div class="big">{tex}</div><div style="color:var(--muted)">{texcount} map(s) baked</div></div>
</div>
"#,
        name = esc(&result.name), fmt = esc(&result.source_format), date = esc(&result.finished_at),
        after_tri = fmt_int(after.triangles), before_tri = fmt_int(before.triangles), red = reduction,
        after_v = fmt_int(after.vertices), before_v = fmt_int(before.vertices),
        after_b = fmt_bytes(after.size_bytes), before_b = fmt_bytes(before.size_bytes),
        tex = if after.texture_size > 0 { format!("{0}×{0}", after.texture_size) } else { "none".into() },
        texcount = result.files.iter().filter(|f| f.kind == "texture").count(),
    )?;

    // LODs
    if !after.lods.is_empty() {
        write!(h, r#"<div class="card" style="margin-bottom:16px"><h2>LOD chain</h2><table><tr><th>Level</th><th>Triangles</th><th>Vertices</th><th>Screen coverage</th></tr>"#)?;
        for l in &after.lods {
            write!(h, "<tr><td>LOD{}</td><td>{}</td><td>{}</td><td>{:.0}%</td></tr>", l.level, fmt_int(l.triangles), fmt_int(l.vertices), l.screen_coverage * 100.0)?;
        }
        write!(h, "</table></div>")?;
    }
    // Problems fixed
    write!(h, r#"<div class="grid"><div class="card"><h2>What was fixed</h2><ul>"#)?;
    if result.problems_fixed.is_empty() {
        write!(h, "<li>Nothing needed fixing. Nice input!</li>")?;
    }
    for p in &result.problems_fixed {
        write!(h, "<li>{}</li>", esc(p))?;
    }
    write!(h, r#"</ul></div><div class="card"><h2>Timings</h2><table>"#)?;
    let mut total = 0.0;
    for (k, v) in &result.timings {
        let v = v.as_f64().unwrap_or(0.0);
        total += v;
        write!(h, "<tr><td>{}</td><td style=\"text-align:right\">{:.1}s</td></tr>", esc(k), v)?;
    }
    write!(h, "<tr><th>Total</th><th style=\"text-align:right\">{total:.1}s</th></tr></table></div></div>")?;
    // Files
    write!(h, r#"<div class="card" style="margin-bottom:16px"><h2>Files</h2><table><tr><th>File</th><th>Kind</th><th>Size</th></tr>"#)?;
    for f in &result.files {
        write!(h, "<tr><td><code>{}</code></td><td>{}</td><td>{}</td></tr>", esc(&f.name), esc(&f.kind), fmt_bytes(f.size_bytes))?;
    }
    write!(h, "</table></div>")?;
    // Textures
    let textures: Vec<_> = result.files.iter().filter(|f| f.kind == "texture").collect();
    if !textures.is_empty() {
        write!(h, r#"<div class="card" style="margin-bottom:16px"><h2>Baked textures</h2><div class="tex">"#)?;
        for t in textures {
            write!(h, r#"<figure style="margin:0"><img src="{0}" alt="{0}" loading="lazy"><figcaption>{0}</figcaption></figure>"#, esc(&t.name))?;
        }
        write!(h, "</div></div>")?;
    }
    // Engine notes
    write!(h, r#"<div class="card" style="margin-bottom:16px"><h2>Using the result</h2><ul>
<li><b>Unity / Godot / Blender:</b> import <code>{n}.glb</code>. Textures, normal map (OpenGL +Y), occlusion and LODs (MSFT_lod) are embedded.</li>
<li><b>Unreal:</b> import <code>{n}.glb</code> (Interchange importer reads LODs and the ORM texture) or the OBJ; collision meshes are named <code>UCX_*</code>.</li>
<li><b>Maya / Cinema 4D:</b> import <code>{n}.obj</code>; the MTL references the PNG textures next to it. <code>{n}_orm.png</code> packs occlusion (R), roughness (G) and metallic (B).</li>
<li>Normal map convention is <b>{conv}</b>. Flip the green channel in your engine if lighting looks inverted.</li>
</ul></div>"#, n = esc(&result.name), conv = esc(&result.normal_convention))?;
    write!(h, "<details class=\"card\"><summary style=\"cursor:pointer;color:var(--muted)\">Recipe used</summary><pre>{}</pre></details>", esc(recipe_json))?;
    write!(h, "<footer>Generated by Polysquish {} · free for everyone</footer></main></body></html>", crate::VERSION)?;
    Ok(h)
}
