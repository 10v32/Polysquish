//! glTF / GLB import: flattens the node hierarchy into one world-space mesh.

use crate::mesh::{Animation, AnimationChannel, Joint, Material, Mesh, Scene, Skeleton, Texture};
use anyhow::{Context, Result};
use glam::{Mat4, Vec2, Vec3};
use std::collections::HashMap;
use std::path::Path;

pub fn load(path: &Path) -> Result<Scene> {
    let (doc, buffers, images) =
        gltf::import(path).with_context(|| format!("failed to parse {}", path.display()))?;

    let mut scene = Scene::default();

    // Textures: glTF texture index -> our texture index (decoded lazily, only if referenced).
    let mut tex_map: HashMap<usize, usize> = HashMap::new();
    let mut get_tex = |scene: &mut Scene, tex: &gltf::Texture| -> Option<usize> {
        let ti = tex.index();
        if let Some(&i) = tex_map.get(&ti) {
            return Some(i);
        }
        let img = images.get(tex.source().index())?;
        let rgba = image_to_rgba(img)?;
        scene.textures.push(Texture {
            name: tex
                .name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("texture_{ti}")),
            image: rgba,
        });
        let idx = scene.textures.len() - 1;
        tex_map.insert(ti, idx);
        Some(idx)
    };

    for m in doc.materials() {
        let pbr = m.pbr_metallic_roughness();
        let mut mat = Material {
            name: m.name().unwrap_or("Material").to_string(),
            base_color: pbr.base_color_factor(),
            metallic: pbr.metallic_factor(),
            roughness: pbr.roughness_factor(),
            emissive: m.emissive_factor(),
            ..Default::default()
        };
        if let Some(t) = pbr.base_color_texture() {
            mat.base_color_tex = get_tex(&mut scene, &t.texture());
        }
        if let Some(t) = pbr.metallic_roughness_texture() {
            mat.metallic_roughness_tex = get_tex(&mut scene, &t.texture());
        }
        if let Some(t) = m.normal_texture() {
            mat.normal_tex = get_tex(&mut scene, &t.texture());
        }
        if let Some(t) = m.emissive_texture() {
            mat.emissive_tex = get_tex(&mut scene, &t.texture());
        }
        scene.materials.push(mat);
    }
    // Default material slot for primitives without one.
    let default_mat = scene.materials.len() as u32;
    scene.materials.push(Material::default());

    // Skeleton: the first skin in the file. Joint order follows the skin's joint list.
    let mut skeleton: Option<Skeleton> = None;
    let mut joint_of_node: HashMap<usize, usize> = HashMap::new();
    let mut skin_index: Option<usize> = None;
    if let Some(skin) = doc.skins().next() {
        skin_index = Some(skin.index());
        let joints: Vec<gltf::Node> = skin.joints().collect();
        for (ji, j) in joints.iter().enumerate() {
            joint_of_node.insert(j.index(), ji);
        }
        // Parent map over all nodes.
        let mut parent_of: HashMap<usize, usize> = HashMap::new();
        for n in doc.nodes() {
            for c in n.children() {
                parent_of.insert(c.index(), n.index());
            }
        }
        // World transforms of all nodes (for joints whose parent is not a joint).
        let mut world: HashMap<usize, Mat4> = HashMap::new();
        fn world_of(n: usize, doc: &gltf::Document, parent_of: &HashMap<usize, usize>, cache: &mut HashMap<usize, Mat4>) -> Mat4 {
            if let Some(m) = cache.get(&n) {
                return *m;
            }
            let local = Mat4::from_cols_array_2d(&doc.nodes().nth(n).unwrap().transform().matrix());
            let m = match parent_of.get(&n) {
                Some(p) => world_of(*p, doc, parent_of, cache) * local,
                None => local,
            };
            cache.insert(n, m);
            m
        }
        let reader = skin.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
        let ibms: Vec<[[f32; 4]; 4]> = reader.read_inverse_bind_matrices().map(|it| it.collect()).unwrap_or_default();
        let mut sk = Skeleton::default();
        for (ji, j) in joints.iter().enumerate() {
            let parent = parent_of.get(&j.index()).and_then(|p| joint_of_node.get(p)).copied();
            let local = if parent.is_some() {
                Mat4::from_cols_array_2d(&j.transform().matrix())
            } else {
                world_of(j.index(), &doc, &parent_of, &mut world)
            };
            sk.joints.push(Joint {
                name: j.name().map(|s| s.to_string()).unwrap_or_else(|| format!("joint_{ji}")),
                parent,
                local: local.to_cols_array_2d(),
                inverse_bind: ibms.get(ji).copied().unwrap_or(Mat4::IDENTITY.to_cols_array_2d()),
            });
        }
        skeleton = Some(sk);
    }
    let mut animations: Vec<Animation> = Vec::new();
    if skeleton.is_some() {
        for (ai, anim) in doc.animations().enumerate() {
            let mut a = Animation { name: anim.name().map(|s| s.to_string()).unwrap_or_else(|| format!("animation_{ai}")), channels: Vec::new() };
            for ch in anim.channels() {
                let Some(&joint) = joint_of_node.get(&ch.target().node().index()) else { continue };
                let reader = ch.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
                let Some(inputs) = reader.read_inputs() else { continue };
                let times: Vec<f32> = inputs.collect();
                let (path, values): (&str, Vec<f32>) = match reader.read_outputs() {
                    Some(gltf::animation::util::ReadOutputs::Translations(t)) => ("translation", t.flat_map(|v| v).collect()),
                    Some(gltf::animation::util::ReadOutputs::Rotations(r)) => ("rotation", r.into_f32().flat_map(|v| v).collect()),
                    Some(gltf::animation::util::ReadOutputs::Scales(sc)) => ("scale", sc.flat_map(|v| v).collect()),
                    _ => continue,
                };
                let interpolation = match ch.sampler().interpolation() {
                    gltf::animation::Interpolation::Linear => "LINEAR",
                    gltf::animation::Interpolation::Step => "STEP",
                    gltf::animation::Interpolation::CubicSpline => "CUBICSPLINE",
                };
                a.channels.push(AnimationChannel { joint, path: path.into(), interpolation: interpolation.into(), times, values });
            }
            if !a.channels.is_empty() {
                animations.push(a);
            }
        }
    }

    let mut merged = Mesh::default();
    let root_scene = doc.default_scene().or_else(|| doc.scenes().next());
    let mut stack: Vec<(gltf::Node, Mat4)> = Vec::new();
    if let Some(s) = root_scene {
        for n in s.nodes() {
            stack.push((n, Mat4::IDENTITY));
        }
    } else {
        for n in doc.nodes() {
            stack.push((n, Mat4::IDENTITY));
        }
    }

    while let Some((node, parent)) = stack.pop() {
        let local = Mat4::from_cols_array_2d(&node.transform().matrix());
        let world = parent * local;
        for child in node.children() {
            stack.push((child, world));
        }
        let Some(mesh) = node.mesh() else { continue };
        // A skinned mesh is bound in the skin's space: its node transform is ignored (glTF spec).
        let skinned = node.skin().map(|s| Some(s.index()) == skin_index).unwrap_or(false);
        let world = if skinned { Mat4::IDENTITY } else { world };
        let normal_mat = world.inverse().transpose();
        for prim in mesh.primitives() {
            let mode = prim.mode();
            if !matches!(
                mode,
                gltf::mesh::Mode::Triangles | gltf::mesh::Mode::TriangleStrip | gltf::mesh::Mode::TriangleFan
            ) {
                continue;
            }
            let reader = prim.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
            let Some(pos_iter) = reader.read_positions() else { continue };
            let mut part = Mesh {
                positions: pos_iter
                    .map(|p| world.transform_point3(Vec3::from(p)))
                    .collect(),
                ..Default::default()
            };
            let n = part.positions.len();
            if let Some(ns) = reader.read_normals() {
                let v: Vec<Vec3> = ns
                    .map(|nn| normal_mat.transform_vector3(Vec3::from(nn)).normalize_or_zero())
                    .collect();
                if v.len() == n {
                    part.normals = v;
                }
            }
            if let Some(uvs) = reader.read_tex_coords(0) {
                let v: Vec<Vec2> = uvs.into_f32().map(Vec2::from).collect();
                if v.len() == n {
                    part.uvs = v;
                }
            }
            if let Some(cols) = reader.read_colors(0) {
                let v: Vec<[f32; 4]> = cols.into_rgba_f32().collect();
                if v.len() == n {
                    part.colors = v;
                }
            }
            if skinned {
                if let (Some(js), Some(ws)) = (reader.read_joints(0), reader.read_weights(0)) {
                    let j: Vec<[u16; 4]> = js.into_u16().collect();
                    let w: Vec<[f32; 4]> = ws.into_f32().collect();
                    if j.len() == n && w.len() == n {
                        part.joints = j;
                        part.weights = w;
                    }
                }
            }
            let raw: Vec<u32> = match reader.read_indices() {
                Some(idx) => idx.into_u32().collect(),
                None => (0..n as u32).collect(),
            };
            part.indices = match mode {
                gltf::mesh::Mode::Triangles => raw,
                gltf::mesh::Mode::TriangleStrip => {
                    let mut out = Vec::with_capacity(raw.len() * 3);
                    for i in 2..raw.len() {
                        if i % 2 == 0 {
                            out.extend_from_slice(&[raw[i - 2], raw[i - 1], raw[i]]);
                        } else {
                            out.extend_from_slice(&[raw[i - 1], raw[i - 2], raw[i]]);
                        }
                    }
                    out
                }
                gltf::mesh::Mode::TriangleFan => {
                    let mut out = Vec::with_capacity(raw.len() * 3);
                    for i in 2..raw.len() {
                        out.extend_from_slice(&[raw[0], raw[i - 1], raw[i]]);
                    }
                    out
                }
                _ => unreachable!(),
            };
            // Negative-determinant transforms flip winding.
            if world.determinant() < 0.0 {
                for t in part.indices.chunks_mut(3) {
                    t.swap(1, 2);
                }
            }
            let mat_id = prim
                .material()
                .index()
                .map(|i| i as u32)
                .unwrap_or(default_mat);
            merged.append(&part, mat_id);
        }
    }
    // Mixed skinned/unskinned parts: vertices without weights bind to joint 0 so the skin stays valid.
    if merged.has_skin() || (!merged.joints.is_empty() && merged.joints.len() == merged.positions.len()) {
        for i in 0..merged.positions.len() {
            if merged.weights[i].iter().all(|w| *w == 0.0) {
                merged.weights[i] = [1.0, 0.0, 0.0, 0.0];
            }
        }
    }
    if !merged.has_skin() {
        skeleton = None;
        animations.clear();
    }
    scene.skeleton = skeleton;
    scene.animations = animations;
    scene.mesh = merged;
    scene.name = doc
        .default_scene()
        .and_then(|s| s.name().map(|s| s.to_string()))
        .unwrap_or_default();
    Ok(scene)
}

fn image_to_rgba(img: &gltf::image::Data) -> Option<image::RgbaImage> {
    use gltf::image::Format as F;
    let (w, h) = (img.width, img.height);
    let px = &img.pixels;
    let mut out = image::RgbaImage::new(w, h);
    let n = (w * h) as usize;
    let ob: &mut [u8] = &mut out;
    match img.format {
        F::R8G8B8A8 => {
            ob.copy_from_slice(&px[..n * 4]);
        }
        F::R8G8B8 => {
            for i in 0..n {
                ob[i * 4..i * 4 + 3].copy_from_slice(&px[i * 3..i * 3 + 3]);
                ob[i * 4 + 3] = 255;
            }
        }
        F::R8 => {
            for i in 0..n {
                let v = px[i];
                ob[i * 4..i * 4 + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        F::R8G8 => {
            for i in 0..n {
                ob[i * 4..i * 4 + 4].copy_from_slice(&[px[i * 2], px[i * 2 + 1], 0, 255]);
            }
        }
        F::R16 | F::R16G16 | F::R16G16B16 | F::R16G16B16A16 | F::R32G32B32FLOAT | F::R32G32B32A32FLOAT => {
            // Rare for colour textures; downconvert by taking the high byte / clamping floats.
            let channels = match img.format {
                F::R16 => 1,
                F::R16G16 => 2,
                F::R16G16B16 => 3,
                F::R16G16B16A16 => 4,
                F::R32G32B32FLOAT => 3,
                _ => 4,
            };
            let is_float = matches!(img.format, F::R32G32B32FLOAT | F::R32G32B32A32FLOAT);
            for i in 0..n {
                let mut rgba = [0u8, 0, 0, 255];
                for c in 0..channels.min(4) {
                    let v = if is_float {
                        let o = (i * channels + c) * 4;
                        let f = f32::from_le_bytes(px[o..o + 4].try_into().ok()?);
                        (f.clamp(0.0, 1.0) * 255.0) as u8
                    } else {
                        let o = (i * channels + c) * 2;
                        px[o + 1]
                    };
                    rgba[c] = v;
                }
                if channels == 1 {
                    rgba[1] = rgba[0];
                    rgba[2] = rgba[0];
                }
                ob[i * 4..i * 4 + 4].copy_from_slice(&rgba);
            }
        }
    }
    Some(out)
}
