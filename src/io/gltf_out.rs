//! GLB writer: LOD chain via MSFT_lod, PBR material with baked textures, optional collision nodes.

use crate::mesh::Mesh;
use anyhow::Result;
use glam::Vec3;
use image::RgbaImage;
use serde_json::{json, Value};
use std::io::Cursor;

pub struct GlbMaterial {
    pub name: String,
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub albedo: Option<RgbaImage>,
    pub normal: Option<RgbaImage>,
    /// R = occlusion, G = roughness, B = metallic.
    pub orm: Option<RgbaImage>,
    pub double_sided: bool,
}

impl Default for GlbMaterial {
    fn default() -> Self {
        Self {
            name: "Material".into(),
            base_color: [1.0; 4],
            metallic: 0.0,
            roughness: 0.6,
            albedo: None,
            normal: None,
            orm: None,
            double_sided: false,
        }
    }
}

pub struct GlbScene<'a> {
    pub name: String,
    /// LOD0 first.
    pub lods: Vec<&'a Mesh>,
    pub screen_coverage: Vec<f32>,
    pub material: GlbMaterial,
    pub collision: Vec<(String, &'a Mesh)>,
    pub scale: f32,
    pub generator_note: String,
}

struct Builder {
    bin: Vec<u8>,
    buffer_views: Vec<Value>,
    accessors: Vec<Value>,
}

impl Builder {
    fn align(&mut self) {
        while self.bin.len() % 4 != 0 {
            self.bin.push(0);
        }
    }
    fn push_view(&mut self, bytes: &[u8], target: Option<u32>) -> usize {
        self.align();
        let offset = self.bin.len();
        self.bin.extend_from_slice(bytes);
        let mut v = json!({ "buffer": 0, "byteOffset": offset, "byteLength": bytes.len() });
        if let Some(t) = target {
            v["target"] = json!(t);
        }
        self.buffer_views.push(v);
        self.buffer_views.len() - 1
    }
    fn push_accessor(&mut self, view: usize, count: usize, ctype: u32, atype: &str, min: Option<Value>, max: Option<Value>) -> usize {
        let mut a = json!({ "bufferView": view, "componentType": ctype, "count": count, "type": atype });
        if let Some(m) = min {
            a["min"] = m;
        }
        if let Some(m) = max {
            a["max"] = m;
        }
        self.accessors.push(a);
        self.accessors.len() - 1
    }
    fn push_mesh(&mut self, mesh: &Mesh, scale: f32, material: Option<usize>, name: &str, meshes: &mut Vec<Value>) -> usize {
        let positions: Vec<[f32; 3]> = mesh.positions.iter().map(|p| (*p * scale).into()).collect();
        let mut mn = Vec3::splat(f32::INFINITY);
        let mut mx = Vec3::splat(f32::NEG_INFINITY);
        for p in &positions {
            mn = mn.min(Vec3::from(*p));
            mx = mx.max(Vec3::from(*p));
        }
        let pv = self.push_view(bytemuck::cast_slice(&positions), Some(34962));
        let pa = self.push_accessor(pv, positions.len(), 5126, "VEC3", Some(json!([mn.x, mn.y, mn.z])), Some(json!([mx.x, mx.y, mx.z])));
        let mut attrs = json!({ "POSITION": pa });
        if mesh.has_normals() {
            let n: Vec<[f32; 3]> = mesh.normals.iter().map(|v| (*v).into()).collect();
            let v = self.push_view(bytemuck::cast_slice(&n), Some(34962));
            attrs["NORMAL"] = json!(self.push_accessor(v, n.len(), 5126, "VEC3", None, None));
        }
        if mesh.has_uvs() {
            let uv: Vec<[f32; 2]> = mesh.uvs.iter().map(|v| (*v).into()).collect();
            let v = self.push_view(bytemuck::cast_slice(&uv), Some(34962));
            attrs["TEXCOORD_0"] = json!(self.push_accessor(v, uv.len(), 5126, "VEC2", None, None));
        }
        if mesh.has_colors() {
            let v = self.push_view(bytemuck::cast_slice(&mesh.colors), Some(34962));
            attrs["COLOR_0"] = json!(self.push_accessor(v, mesh.colors.len(), 5126, "VEC4", None, None));
        }
        let iv = self.push_view(bytemuck::cast_slice(&mesh.indices), Some(34963));
        let ia = self.push_accessor(iv, mesh.indices.len(), 5125, "SCALAR", None, None);
        let mut prim = json!({ "attributes": attrs, "indices": ia, "mode": 4 });
        if let Some(m) = material {
            prim["material"] = json!(m);
        }
        meshes.push(json!({ "name": name, "primitives": [prim] }));
        meshes.len() - 1
    }
    fn push_image(&mut self, img: &RgbaImage, name: &str, images: &mut Vec<Value>) -> Result<usize> {
        let mut png = Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png)?;
        let view = self.push_view(png.get_ref(), None);
        images.push(json!({ "name": name, "mimeType": "image/png", "bufferView": view }));
        Ok(images.len() - 1)
    }
}

pub fn encode(scene: &GlbScene) -> Result<Vec<u8>> {
    let mut b = Builder { bin: Vec::new(), buffer_views: Vec::new(), accessors: Vec::new() };
    let mut images = Vec::new();
    let mut textures = Vec::new();
    let mut materials = Vec::new();
    let mut meshes = Vec::new();
    let mut nodes = Vec::new();

    // Material
    let m = &scene.material;
    let mut mat = json!({
        "name": m.name,
        "pbrMetallicRoughness": {
            "baseColorFactor": m.base_color,
            "metallicFactor": m.metallic,
            "roughnessFactor": m.roughness
        },
        "doubleSided": m.double_sided
    });
    let mut add_tex = |b: &mut Builder, img: &RgbaImage, name: &str| -> Result<usize> {
        let ii = b.push_image(img, name, &mut images)?;
        textures.push(json!({ "source": ii, "sampler": 0 }));
        Ok(textures.len() - 1)
    };
    if let Some(img) = &m.albedo {
        let t = add_tex(&mut b, img, &format!("{}_albedo", scene.name))?;
        mat["pbrMetallicRoughness"]["baseColorTexture"] = json!({ "index": t });
    }
    if let Some(img) = &m.orm {
        let t = add_tex(&mut b, img, &format!("{}_orm", scene.name))?;
        mat["pbrMetallicRoughness"]["metallicRoughnessTexture"] = json!({ "index": t });
        mat["occlusionTexture"] = json!({ "index": t });
        // When a texture drives metal/rough, factors act as multipliers: use 1.0.
        mat["pbrMetallicRoughness"]["metallicFactor"] = json!(1.0);
        mat["pbrMetallicRoughness"]["roughnessFactor"] = json!(1.0);
    }
    if let Some(img) = &m.normal {
        let t = add_tex(&mut b, img, &format!("{}_normal", scene.name))?;
        mat["normalTexture"] = json!({ "index": t });
    }
    materials.push(mat);

    // LOD meshes and nodes
    let mut lod_nodes = Vec::new();
    for (i, mesh) in scene.lods.iter().enumerate() {
        let name = if scene.lods.len() > 1 { format!("{}_LOD{}", scene.name, i) } else { scene.name.clone() };
        let mi = b.push_mesh(mesh, scene.scale, Some(0), &name, &mut meshes);
        nodes.push(json!({ "name": name, "mesh": mi }));
        lod_nodes.push(nodes.len() - 1);
    }
    let mut extensions_used: Vec<&str> = Vec::new();
    if lod_nodes.len() > 1 {
        let ids: Vec<usize> = lod_nodes[1..].to_vec();
        let cov: Vec<f32> = scene.screen_coverage.clone();
        nodes[lod_nodes[0]]["extensions"] = json!({ "MSFT_lod": { "ids": ids } });
        nodes[lod_nodes[0]]["extras"] = json!({ "MSFT_screencoverage": cov });
        extensions_used.push("MSFT_lod");
    }
    // Collision nodes
    let mut scene_nodes = vec![lod_nodes[0]];
    for (name, mesh) in &scene.collision {
        let mi = b.push_mesh(mesh, scene.scale, None, name, &mut meshes);
        nodes.push(json!({ "name": name, "mesh": mi, "extras": { "collision": true } }));
        scene_nodes.push(nodes.len() - 1);
    }

    b.align();
    let mut root = json!({
        "asset": { "version": "2.0", "generator": format!("Polysquish {}", crate::VERSION), "extras": { "note": scene.generator_note } },
        "scene": 0,
        "scenes": [{ "name": scene.name, "nodes": scene_nodes }],
        "nodes": nodes,
        "meshes": meshes,
        "materials": materials,
        "accessors": b.accessors,
        "bufferViews": b.buffer_views,
        "buffers": [{ "byteLength": b.bin.len() }],
        "samplers": [{ "magFilter": 9729, "minFilter": 9987, "wrapS": 10497, "wrapT": 10497 }],
    });
    if !images.is_empty() {
        root["images"] = json!(images);
        root["textures"] = json!(textures);
    }
    if !extensions_used.is_empty() {
        root["extensionsUsed"] = json!(extensions_used);
    }
    let mut json_bytes = serde_json::to_vec(&root)?;
    while json_bytes.len() % 4 != 0 {
        json_bytes.push(b' ');
    }
    let total = 12 + 8 + json_bytes.len() + 8 + b.bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json_bytes);
    out.extend_from_slice(&(b.bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&b.bin);
    Ok(out)
}

/// Convenience: a single mesh with an optional vertex-colour-only material, for previews.
pub fn encode_simple(name: &str, mesh: &Mesh, material: GlbMaterial) -> Result<Vec<u8>> {
    encode(&GlbScene {
        name: name.to_string(),
        lods: vec![mesh],
        screen_coverage: vec![],
        material,
        collision: vec![],
        scale: 1.0,
        generator_note: String::new(),
    })
}
