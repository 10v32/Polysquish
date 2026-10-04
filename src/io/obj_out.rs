//! OBJ + MTL writer (the universal fallback for Blender, Maya, Cinema 4D and friends).

use crate::mesh::Mesh;
use anyhow::Result;
use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

pub struct ObjMaterialFiles {
    pub albedo: Option<String>,
    pub normal: Option<String>,
    pub ao: Option<String>,
    pub orm: Option<String>,
    pub base_color: [f32; 4],
    pub roughness: f32,
    pub metallic: f32,
}

pub fn write_obj(path: &Path, mesh: &Mesh, object_name: &str, mtl_file: Option<&str>, material_name: &str, scale: f32) -> Result<()> {
    let mut s = String::with_capacity(mesh.vertex_count() * 64);
    writeln!(s, "# Exported by Polysquish {}", crate::VERSION)?;
    if let Some(m) = mtl_file {
        writeln!(s, "mtllib {m}")?;
    }
    writeln!(s, "o {object_name}")?;
    for p in &mesh.positions {
        writeln!(s, "v {} {} {}", p.x * scale, p.y * scale, p.z * scale)?;
    }
    for uv in &mesh.uvs {
        writeln!(s, "vt {} {}", uv.x, 1.0 - uv.y)?;
    }
    for n in &mesh.normals {
        writeln!(s, "vn {} {} {}", n.x, n.y, n.z)?;
    }
    writeln!(s, "usemtl {material_name}")?;
    writeln!(s, "s 1")?;
    let (has_uv, has_n) = (mesh.has_uvs(), mesh.has_normals());
    for t in 0..mesh.triangle_count() {
        let [a, b, c] = mesh.tri(t);
        let f = |i: u32| -> String {
            let i = i + 1;
            match (has_uv, has_n) {
                (true, true) => format!("{i}/{i}/{i}"),
                (true, false) => format!("{i}/{i}"),
                (false, true) => format!("{i}//{i}"),
                (false, false) => format!("{i}"),
            }
        };
        writeln!(s, "f {} {} {}", f(a), f(b), f(c))?;
    }
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    file.write_all(s.as_bytes())?;
    Ok(())
}

pub fn write_mtl(path: &Path, material_name: &str, files: &ObjMaterialFiles) -> Result<()> {
    let mut s = String::new();
    writeln!(s, "# Exported by Polysquish {}", crate::VERSION)?;
    writeln!(s, "newmtl {material_name}")?;
    let c = files.base_color;
    writeln!(s, "Kd {} {} {}", c[0], c[1], c[2])?;
    writeln!(s, "Ka 1.0 1.0 1.0")?;
    writeln!(s, "Ks 0.1 0.1 0.1")?;
    writeln!(s, "Ns {}", ((1.0 - files.roughness).clamp(0.0, 1.0) * 1000.0).max(1.0))?;
    writeln!(s, "d {}", c[3])?;
    writeln!(s, "illum 2")?;
    // PBR extensions understood by Blender and many importers.
    writeln!(s, "Pr {}", files.roughness)?;
    writeln!(s, "Pm {}", files.metallic)?;
    if let Some(a) = &files.albedo {
        writeln!(s, "map_Kd {a}")?;
    }
    if let Some(n) = &files.normal {
        writeln!(s, "norm {n}")?;
        writeln!(s, "map_Bump -bm 1.0 {n}")?;
    }
    if let Some(ao) = &files.ao {
        writeln!(s, "map_Ka {ao}")?;
    }
    if let Some(orm) = &files.orm {
        writeln!(s, "map_Pr -imfchan g {orm}")?;
        writeln!(s, "map_Pm -imfchan b {orm}")?;
    }
    std::fs::write(path, s)?;
    Ok(())
}
