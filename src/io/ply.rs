//! Fast PLY import (ascii, binary little/big endian) supporting positions, normals,
//! vertex colours and UVs, plus `vertex_indices` faces (triangulated by fan).

use crate::mesh::{Mesh, Scene};
use anyhow::{anyhow, bail, Context, Result};
use glam::{Vec2, Vec3};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ty {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Ty {
    fn parse(s: &str) -> Result<Ty> {
        Ok(match s {
            "char" | "int8" => Ty::I8,
            "uchar" | "uint8" => Ty::U8,
            "short" | "int16" => Ty::I16,
            "ushort" | "uint16" => Ty::U16,
            "int" | "int32" => Ty::I32,
            "uint" | "uint32" => Ty::U32,
            "float" | "float32" => Ty::F32,
            "double" | "float64" => Ty::F64,
            other => bail!("unknown PLY type {other}"),
        })
    }
    fn size(self) -> usize {
        match self {
            Ty::I8 | Ty::U8 => 1,
            Ty::I16 | Ty::U16 => 2,
            Ty::I32 | Ty::U32 | Ty::F32 => 4,
            Ty::F64 => 8,
        }
    }
}

#[derive(Clone, Debug)]
struct Prop {
    name: String,
    ty: Ty,
    list_count_ty: Option<Ty>,
}

#[derive(Clone, Debug)]
struct Element {
    name: String,
    count: usize,
    props: Vec<Prop>,
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Ascii,
    BinaryLe,
    BinaryBe,
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
    be: bool,
}

impl<'a> Cursor<'a> {
    #[inline]
    fn read(&mut self, ty: Ty) -> Result<f64> {
        let n = ty.size();
        if self.pos + n > self.data.len() {
            bail!("unexpected end of PLY data");
        }
        let b = &self.data[self.pos..self.pos + n];
        self.pos += n;
        macro_rules! rd {
            ($t:ty) => {{
                let arr: [u8; std::mem::size_of::<$t>()] = b.try_into().unwrap();
                if self.be {
                    <$t>::from_be_bytes(arr) as f64
                } else {
                    <$t>::from_le_bytes(arr) as f64
                }
            }};
        }
        Ok(match ty {
            Ty::I8 => rd!(i8),
            Ty::U8 => rd!(u8),
            Ty::I16 => rd!(i16),
            Ty::U16 => rd!(u16),
            Ty::I32 => rd!(i32),
            Ty::U32 => rd!(u32),
            Ty::F32 => rd!(f32),
            Ty::F64 => rd!(f64),
        })
    }
}

pub fn load(path: &Path) -> Result<Scene> {
    let file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let data = unsafe { memmap2::Mmap::map(&file)? };
    let data: &[u8] = &data;

    // --- header ---
    let header_end = find_header_end(data).ok_or_else(|| anyhow!("PLY header not terminated"))?;
    let header = std::str::from_utf8(&data[..header_end]).context("PLY header is not UTF-8")?;
    let mut format = Format::Ascii;
    let mut elements: Vec<Element> = Vec::new();
    for line in header.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("ply") | Some("comment") | Some("obj_info") | None => {}
            Some("format") => {
                format = match it.next() {
                    Some("ascii") => Format::Ascii,
                    Some("binary_little_endian") => Format::BinaryLe,
                    Some("binary_big_endian") => Format::BinaryBe,
                    other => bail!("unknown PLY format {other:?}"),
                }
            }
            Some("element") => {
                let name = it.next().ok_or_else(|| anyhow!("bad element line"))?.to_string();
                let count: usize = it.next().ok_or_else(|| anyhow!("bad element line"))?.parse()?;
                elements.push(Element { name, count, props: vec![] });
            }
            Some("property") => {
                let el = elements.last_mut().ok_or_else(|| anyhow!("property before element"))?;
                let a = it.next().ok_or_else(|| anyhow!("bad property"))?;
                if a == "list" {
                    let cty = Ty::parse(it.next().ok_or_else(|| anyhow!("bad list"))?)?;
                    let ty = Ty::parse(it.next().ok_or_else(|| anyhow!("bad list"))?)?;
                    let name = it.next().ok_or_else(|| anyhow!("bad list"))?.to_string();
                    el.props.push(Prop { name, ty, list_count_ty: Some(cty) });
                } else {
                    let ty = Ty::parse(a)?;
                    let name = it.next().ok_or_else(|| anyhow!("bad property"))?.to_string();
                    el.props.push(Prop { name, ty, list_count_ty: None });
                }
            }
            Some("end_header") => break,
            Some(other) => log::debug!("ignoring PLY header line {other}"),
        }
    }

    let mut mesh = Mesh::default();
    let body = &data[header_end..];

    match format {
        Format::Ascii => parse_ascii(body, &elements, &mut mesh)?,
        Format::BinaryLe | Format::BinaryBe => {
            let mut cur = Cursor { data: body, pos: 0, be: format == Format::BinaryBe };
            parse_binary(&mut cur, &elements, &mut mesh)?
        }
    }
    Ok(Scene { mesh, ..Default::default() })
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    let needle = b"end_header";
    let limit = data.len().min(1 << 20);
    let pos = data[..limit].windows(needle.len()).position(|w| w == needle)?;
    let mut end = pos + needle.len();
    // consume to end of line (\n or \r\n)
    while end < data.len() && data[end] != b'\n' {
        end += 1;
    }
    Some((end + 1).min(data.len()))
}

struct VertexLayout {
    x: Option<usize>,
    y: Option<usize>,
    z: Option<usize>,
    nx: Option<usize>,
    ny: Option<usize>,
    nz: Option<usize>,
    r: Option<usize>,
    g: Option<usize>,
    b: Option<usize>,
    a: Option<usize>,
    u: Option<usize>,
    v: Option<usize>,
    color_is_byte: bool,
}

fn layout(el: &Element) -> VertexLayout {
    let find = |names: &[&str]| el.props.iter().position(|p| names.contains(&p.name.as_str()));
    let r = find(&["red", "r", "diffuse_red"]);
    let color_is_byte = r
        .map(|i| matches!(el.props[i].ty, Ty::U8 | Ty::I8 | Ty::U16 | Ty::I16 | Ty::I32 | Ty::U32))
        .unwrap_or(true);
    VertexLayout {
        x: find(&["x"]),
        y: find(&["y"]),
        z: find(&["z"]),
        nx: find(&["nx"]),
        ny: find(&["ny"]),
        nz: find(&["nz"]),
        r,
        g: find(&["green", "g", "diffuse_green"]),
        b: find(&["blue", "b", "diffuse_blue"]),
        a: find(&["alpha", "a"]),
        u: find(&["u", "s", "texture_u", "texture_s"]),
        v: find(&["v", "t", "texture_v", "texture_t"]),
        color_is_byte,
    }
}

fn push_vertex(mesh: &mut Mesh, lay: &VertexLayout, vals: &[f64]) {
    let g = |i: Option<usize>| i.map(|i| vals[i] as f32).unwrap_or(0.0);
    mesh.positions.push(Vec3::new(g(lay.x), g(lay.y), g(lay.z)));
    if lay.nx.is_some() {
        mesh.normals.push(Vec3::new(g(lay.nx), g(lay.ny), g(lay.nz)));
    }
    if lay.r.is_some() {
        let s = if lay.color_is_byte { 1.0 / 255.0 } else { 1.0 };
        let a = if lay.a.is_some() { g(lay.a) * s } else { 1.0 };
        mesh.colors.push([g(lay.r) * s, g(lay.g) * s, g(lay.b) * s, a]);
    }
    if lay.u.is_some() {
        mesh.uvs.push(Vec2::new(g(lay.u), 1.0 - g(lay.v)));
    }
}

fn push_face(mesh: &mut Mesh, idx: &[u32]) {
    if idx.len() < 3 {
        return;
    }
    for k in 1..idx.len() - 1 {
        mesh.indices.push(idx[0]);
        mesh.indices.push(idx[k]);
        mesh.indices.push(idx[k + 1]);
    }
}

fn parse_binary(cur: &mut Cursor, elements: &[Element], mesh: &mut Mesh) -> Result<()> {
    let mut vals: Vec<f64> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    for el in elements {
        match el.name.as_str() {
            "vertex" => {
                let lay = layout(el);
                mesh.positions.reserve(el.count);
                for _ in 0..el.count {
                    vals.clear();
                    for p in &el.props {
                        if let Some(cty) = p.list_count_ty {
                            let n = cur.read(cty)? as usize;
                            for _ in 0..n {
                                cur.read(p.ty)?;
                            }
                            vals.push(0.0);
                        } else {
                            vals.push(cur.read(p.ty)?);
                        }
                    }
                    push_vertex(mesh, &lay, &vals);
                }
            }
            "face" => {
                mesh.indices.reserve(el.count * 3);
                for _ in 0..el.count {
                    for p in &el.props {
                        if let Some(cty) = p.list_count_ty {
                            let n = cur.read(cty)? as usize;
                            if p.name == "vertex_indices" || p.name == "vertex_index" {
                                idx.clear();
                                for _ in 0..n {
                                    idx.push(cur.read(p.ty)? as u32);
                                }
                                push_face(mesh, &idx);
                            } else {
                                for _ in 0..n {
                                    cur.read(p.ty)?;
                                }
                            }
                        } else {
                            cur.read(p.ty)?;
                        }
                    }
                }
            }
            _ => {
                // skip unknown element
                for _ in 0..el.count {
                    for p in &el.props {
                        if let Some(cty) = p.list_count_ty {
                            let n = cur.read(cty)? as usize;
                            for _ in 0..n {
                                cur.read(p.ty)?;
                            }
                        } else {
                            cur.read(p.ty)?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn parse_ascii(body: &[u8], elements: &[Element], mesh: &mut Mesh) -> Result<()> {
    let text = std::str::from_utf8(body).context("ASCII PLY body is not UTF-8")?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let mut vals: Vec<f64> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    for el in elements {
        let lay = layout(el);
        for _ in 0..el.count {
            let line = lines.next().ok_or_else(|| anyhow!("PLY: unexpected end of file"))?;
            let mut toks = line.split_ascii_whitespace();
            match el.name.as_str() {
                "vertex" => {
                    vals.clear();
                    for p in &el.props {
                        if p.list_count_ty.is_some() {
                            let n: usize = toks.next().unwrap_or("0").parse().unwrap_or(0);
                            for _ in 0..n {
                                toks.next();
                            }
                            vals.push(0.0);
                        } else {
                            vals.push(toks.next().unwrap_or("0").parse().unwrap_or(0.0));
                        }
                    }
                    push_vertex(mesh, &lay, &vals);
                }
                "face" => {
                    for p in &el.props {
                        if p.list_count_ty.is_some() {
                            let n: usize = toks.next().unwrap_or("0").parse().unwrap_or(0);
                            if p.name == "vertex_indices" || p.name == "vertex_index" {
                                idx.clear();
                                for _ in 0..n {
                                    idx.push(toks.next().unwrap_or("0").parse().unwrap_or(0));
                                }
                                push_face(mesh, &idx);
                            } else {
                                for _ in 0..n {
                                    toks.next();
                                }
                            }
                        } else {
                            toks.next();
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Write a binary little-endian PLY with positions, normals (if any) and colours (if any).
pub fn save_binary(path: &Path, mesh: &Mesh) -> Result<()> {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let has_n = mesh.has_normals();
    let has_c = mesh.has_colors();
    let has_uv = mesh.has_uvs();
    write!(f, "ply\nformat binary_little_endian 1.0\ncomment Polysquish {}\n", crate::VERSION)?;
    write!(f, "element vertex {}\nproperty float x\nproperty float y\nproperty float z\n", mesh.vertex_count())?;
    if has_n {
        write!(f, "property float nx\nproperty float ny\nproperty float nz\n")?;
    }
    if has_c {
        write!(f, "property uchar red\nproperty uchar green\nproperty uchar blue\nproperty uchar alpha\n")?;
    }
    if has_uv {
        write!(f, "property float s\nproperty float t\n")?;
    }
    write!(f, "element face {}\nproperty list uchar uint vertex_indices\nend_header\n", mesh.triangle_count())?;
    for i in 0..mesh.vertex_count() {
        let p = mesh.positions[i];
        f.write_all(&p.x.to_le_bytes())?;
        f.write_all(&p.y.to_le_bytes())?;
        f.write_all(&p.z.to_le_bytes())?;
        if has_n {
            let n = mesh.normals[i];
            f.write_all(&n.x.to_le_bytes())?;
            f.write_all(&n.y.to_le_bytes())?;
            f.write_all(&n.z.to_le_bytes())?;
        }
        if has_c {
            let c = mesh.colors[i];
            f.write_all(&[
                (c[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                (c[1].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                (c[2].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                (c[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
            ])?;
        }
        if has_uv {
            let uv = mesh.uvs[i];
            f.write_all(&uv.x.to_le_bytes())?;
            f.write_all(&(1.0 - uv.y).to_le_bytes())?;
        }
    }
    for t in 0..mesh.triangle_count() {
        let [a, b, c] = mesh.tri(t);
        f.write_all(&[3u8])?;
        f.write_all(&a.to_le_bytes())?;
        f.write_all(&b.to_le_bytes())?;
        f.write_all(&c.to_le_bytes())?;
    }
    f.flush()?;
    Ok(())
}
