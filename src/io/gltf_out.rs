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

/// An additional mesh node with its own material (e.g. an imposter card set).
pub struct ExtraNode<'a> {
    pub name: String,
    pub mesh: &'a Mesh,
    pub material: GlbMaterial,
    /// Alpha-masked (cutout) material.
    pub alpha_mask: bool,
    /// Append this node to the LOD chain (after the last LOD).
    pub as_last_lod: bool,
}

pub struct GlbScene<'a> {
    pub name: String,
    /// Rig: when set and the LOD meshes carry joints/weights, a skin + joint nodes + animations are written.
    pub skeleton: Option<&'a crate::mesh::Skeleton>,
    pub animations: &'a [crate::mesh::Animation],
    /// LOD0 first.
    pub lods: Vec<&'a Mesh>,
    pub screen_coverage: Vec<f32>,
    pub material: GlbMaterial,
    /// Additional materials keyed by the mesh `material_ids` they serve (multi-material export).
    /// Triangles whose id is not listed use `material`.
    pub extra_materials: Vec<(u32, GlbMaterial)>,
    pub collision: Vec<(String, &'a Mesh)>,
    pub scale: f32,
    pub generator_note: String,
    pub extras: Vec<ExtraNode<'a>>,
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
        self.push_mesh_multi(mesh, scale, material, &std::collections::HashMap::new(), name, meshes)
    }

    /// Like `push_mesh`, but triangles whose material id appears in `lookup` go into their own
    /// primitive using that glTF material index.
    fn push_mesh_multi(&mut self, mesh: &Mesh, scale: f32, material: Option<usize>, lookup: &std::collections::HashMap<u32, usize>, name: &str, meshes: &mut Vec<Value>) -> usize {
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
        if mesh.has_skin() {
            let v = self.push_view(bytemuck::cast_slice(&mesh.joints), Some(34962));
            attrs["JOINTS_0"] = json!(self.push_accessor(v, mesh.joints.len(), 5123, "VEC4", None, None));
            let v = self.push_view(bytemuck::cast_slice(&mesh.weights), Some(34962));
            attrs["WEIGHTS_0"] = json!(self.push_accessor(v, mesh.weights.len(), 5126, "VEC4", None, None));
        }
        // Group triangles by glTF material index.
        let mut groups: Vec<(Option<usize>, Vec<u32>)> = Vec::new();
        if lookup.is_empty() || mesh.material_ids.is_empty() {
            groups.push((material, mesh.indices.clone()));
        } else {
            let mut by: std::collections::BTreeMap<Option<usize>, Vec<u32>> = std::collections::BTreeMap::new();
            for t in 0..mesh.triangle_count() {
                let gm = lookup.get(&mesh.material_ids[t]).copied().or(material);
                by.entry(gm).or_default().extend_from_slice(&mesh.indices[t * 3..t * 3 + 3]);
            }
            groups.extend(by.into_iter());
        }
        let mut prims = Vec::new();
        for (gm, idx) in &groups {
            let iv = self.push_view(bytemuck::cast_slice(idx), Some(34963));
            let ia = self.push_accessor(iv, idx.len(), 5125, "SCALAR", None, None);
            let mut prim = json!({ "attributes": attrs.clone(), "indices": ia, "mode": 4 });
            if let Some(m) = gm {
                prim["material"] = json!(m);
            }
            prims.push(prim);
        }
        meshes.push(json!({ "name": name, "primitives": prims }));
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

    // Materials
    fn build_material(b: &mut Builder, images: &mut Vec<Value>, textures: &mut Vec<Value>, m: &GlbMaterial, prefix: &str, alpha_mask: bool) -> Result<Value> {
        let mut mat = json!({
            "name": m.name,
            "pbrMetallicRoughness": {
                "baseColorFactor": m.base_color,
                "metallicFactor": m.metallic,
                "roughnessFactor": m.roughness
            },
            "doubleSided": m.double_sided
        });
        if alpha_mask {
            mat["alphaMode"] = json!("MASK");
            mat["alphaCutoff"] = json!(0.5);
        }
        let mut add_tex = |b: &mut Builder, img: &RgbaImage, name: &str| -> Result<usize> {
            let ii = b.push_image(img, name, images)?;
            textures.push(json!({ "source": ii, "sampler": 0 }));
            Ok(textures.len() - 1)
        };
        if let Some(img) = &m.albedo {
            let t = add_tex(b, img, &format!("{prefix}_albedo"))?;
            mat["pbrMetallicRoughness"]["baseColorTexture"] = json!({ "index": t });
        }
        if let Some(img) = &m.orm {
            let t = add_tex(b, img, &format!("{prefix}_orm"))?;
            mat["pbrMetallicRoughness"]["metallicRoughnessTexture"] = json!({ "index": t });
            mat["occlusionTexture"] = json!({ "index": t });
            mat["pbrMetallicRoughness"]["metallicFactor"] = json!(1.0);
            mat["pbrMetallicRoughness"]["roughnessFactor"] = json!(1.0);
        }
        if let Some(img) = &m.normal {
            let t = add_tex(b, img, &format!("{prefix}_normal"))?;
            mat["normalTexture"] = json!({ "index": t });
        }
        Ok(mat)
    }
    let mat = build_material(&mut b, &mut images, &mut textures, &scene.material, &scene.name, false)?;
    materials.push(mat);
    let mut lookup: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for (k, (mid, gm)) in scene.extra_materials.iter().enumerate() {
        let mat = build_material(&mut b, &mut images, &mut textures, gm, &format!("{}_{}", scene.name, k + 1), false)?;
        materials.push(mat);
        lookup.insert(*mid, materials.len() - 1);
    }

    // LOD meshes and nodes
    let mut lod_nodes = Vec::new();
    for (i, mesh) in scene.lods.iter().enumerate() {
        let name = if scene.lods.len() > 1 { format!("{}_LOD{}", scene.name, i) } else { scene.name.clone() };
        let mi = b.push_mesh_multi(mesh, scene.scale, Some(0), &lookup, &name, &mut meshes);
        nodes.push(json!({ "name": name, "mesh": mi }));
        lod_nodes.push(nodes.len() - 1);
    }
    // Rig: joint nodes, skin, animations.
    let mut skins = Vec::new();
    let mut animations_json = Vec::new();
    let mut joint_root_nodes: Vec<usize> = Vec::new();
    if let Some(sk) = scene.skeleton.filter(|_| scene.lods.iter().any(|m| m.has_skin()) && !scene.lods[0].joints.is_empty()) {
        let base = nodes.len();
        for (ji, j) in sk.joints.iter().enumerate() {
            let m = glam::Mat4::from_cols_array_2d(&j.local);
            let (sc, rot, tr) = m.to_scale_rotation_translation();
            let mut tr = tr;
            if j.parent.is_none() {
                tr *= scene.scale;
            } else {
                tr *= scene.scale;
            }
            let mut node = json!({ "name": j.name, "translation": [tr.x, tr.y, tr.z], "rotation": [rot.x, rot.y, rot.z, rot.w], "scale": [sc.x, sc.y, sc.z] });
            let children: Vec<usize> = sk.joints.iter().enumerate().filter(|(_, c)| c.parent == Some(ji)).map(|(ci, _)| base + ci).collect();
            if !children.is_empty() {
                node["children"] = json!(children);
            }
            nodes.push(node);
            if j.parent.is_none() {
                joint_root_nodes.push(base + ji);
            }
        }
        // Inverse bind matrices (translation part scaled).
        let ibm: Vec<[[f32; 4]; 4]> = sk
            .joints
            .iter()
            .map(|j| {
                let mut m = j.inverse_bind;
                m[3][0] *= scene.scale;
                m[3][1] *= scene.scale;
                m[3][2] *= scene.scale;
                m
            })
            .collect();
        let flat: Vec<f32> = ibm.iter().flat_map(|m| m.iter().flat_map(|c| c.iter().copied())).collect();
        let view = b.push_view(bytemuck::cast_slice(&flat), None);
        let acc = b.push_accessor(view, ibm.len(), 5126, "MAT4", None, None);
        let joint_nodes: Vec<usize> = (0..sk.joints.len()).map(|i| base + i).collect();
        let mut skin = json!({ "name": format!("{}_skin", scene.name), "inverseBindMatrices": acc, "joints": joint_nodes });
        if let Some(r) = joint_root_nodes.first() {
            skin["skeleton"] = json!(r);
        }
        skins.push(skin);
        for &ln in &lod_nodes {
            nodes[ln]["skin"] = json!(0);
        }
        for anim in scene.animations {
            let mut samplers = Vec::new();
            let mut channels = Vec::new();
            for ch in &anim.channels {
                if ch.joint >= sk.joints.len() || ch.times.is_empty() {
                    continue;
                }
                let comps = if ch.path == "rotation" { 4 } else { 3 };
                let per_key = if ch.interpolation == "CUBICSPLINE" { comps * 3 } else { comps };
                if ch.values.len() != ch.times.len() * per_key {
                    continue;
                }
                let tmin = ch.times.iter().cloned().fold(f32::INFINITY, f32::min);
                let tmax = ch.times.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                let tv = b.push_view(bytemuck::cast_slice(&ch.times), None);
                let ta = b.push_accessor(tv, ch.times.len(), 5126, "SCALAR", Some(json!([tmin])), Some(json!([tmax])));
                let mut vals = ch.values.clone();
                if ch.path == "translation" {
                    for v in vals.iter_mut() {
                        *v *= scene.scale;
                    }
                }
                let vv = b.push_view(bytemuck::cast_slice(&vals), None);
                let va = b.push_accessor(vv, vals.len() / comps, 5126, if comps == 4 { "VEC4" } else { "VEC3" }, None, None);
                samplers.push(json!({ "input": ta, "output": va, "interpolation": ch.interpolation }));
                channels.push(json!({ "sampler": samplers.len() - 1, "target": { "node": base + ch.joint, "path": ch.path } }));
            }
            if !channels.is_empty() {
                animations_json.push(json!({ "name": anim.name, "samplers": samplers, "channels": channels }));
            }
        }
    }
    // Extra nodes (imposter cards etc.) with their own materials.
    let mut extra_scene_nodes: Vec<usize> = Vec::new();
    for ex in &scene.extras {
        let mat = build_material(&mut b, &mut images, &mut textures, &ex.material, &ex.name, ex.alpha_mask)?;
        materials.push(mat);
        let mi = b.push_mesh(ex.mesh, scene.scale, Some(materials.len() - 1), &ex.name, &mut meshes);
        nodes.push(json!({ "name": ex.name, "mesh": mi }));
        if ex.as_last_lod {
            lod_nodes.push(nodes.len() - 1);
        } else {
            extra_scene_nodes.push(nodes.len() - 1);
        }
    }
    let mut extensions_used: Vec<&str> = Vec::new();
    if lod_nodes.len() > 1 {
        let ids: Vec<usize> = lod_nodes[1..].to_vec();
        let mut cov: Vec<f32> = scene.screen_coverage.clone();
        while cov.len() < lod_nodes.len() {
            let last = cov.last().copied().unwrap_or(1.0);
            cov.push(last * 0.5);
        }
        cov.truncate(lod_nodes.len());
        nodes[lod_nodes[0]]["extensions"] = json!({ "MSFT_lod": { "ids": ids } });
        nodes[lod_nodes[0]]["extras"] = json!({ "MSFT_screencoverage": cov });
        extensions_used.push("MSFT_lod");
    }
    // Collision nodes
    let mut scene_nodes = vec![lod_nodes[0]];
    scene_nodes.extend(joint_root_nodes.iter().copied());
    scene_nodes.extend(extra_scene_nodes);
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
    if !skins.is_empty() {
        root["skins"] = json!(skins);
    }
    if !animations_json.is_empty() {
        root["animations"] = json!(animations_json);
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
        skeleton: None,
        animations: &[],
        lods: vec![mesh],
        screen_coverage: vec![],
        material,
        extra_materials: vec![],
        collision: vec![],
        scale: 1.0,
        generator_note: String::new(),
        extras: vec![],
    })
}
