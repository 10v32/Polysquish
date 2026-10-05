//! Native binary FBX 7.4 writer (no Autodesk SDK).
//!
//! Writes a self-contained FBX 7400 binary file with LOD groups, polygon (quad-preserving)
//! geometry, a Phong material with file textures, plain collision meshes, and optionally a
//! skin (Deformer/Cluster per joint) plus animation stacks for joint translation / rotation /
//! scale. Everything is built as an in-memory node tree and serialised in one pass.

use crate::mesh::{Animation, Mesh, Skeleton, NO_VERTEX};
use anyhow::{bail, Result};
use glam::{EulerRot, Mat4, Quat, Vec3};
use std::path::Path;

// --------------------------------------------------------------------------------------------
// Public API
// --------------------------------------------------------------------------------------------

pub struct FbxMaterial {
    pub name: String,
    pub base_color: [f32; 4],
    pub roughness: f32,
    pub metallic: f32,
    pub albedo_file: Option<String>,
    pub normal_file: Option<String>,
    pub ao_file: Option<String>,
    pub orm_file: Option<String>,
}

impl Default for FbxMaterial {
    fn default() -> Self {
        Self {
            name: "Material".into(),
            base_color: [0.8, 0.8, 0.8, 1.0],
            roughness: 0.6,
            metallic: 0.0,
            albedo_file: None,
            normal_file: None,
            ao_file: None,
            orm_file: None,
        }
    }
}

pub struct FbxLod<'a> {
    pub name: String,
    pub mesh: &'a Mesh,
}

pub struct FbxScene<'a> {
    pub name: String,
    /// LOD0 first. With more than one entry the LODs become children of an FBX LodGroup.
    pub lods: Vec<FbxLod<'a>>,
    pub material: FbxMaterial,
    /// Extra plain mesh nodes (e.g. `UCX_name_01`) placed at the scene root.
    pub collision: Vec<(String, &'a Mesh)>,
    /// Multiplier applied to all positions (and to translations of joints / animations).
    pub scale: f32,
    /// When `Some` and `lods[0].mesh.has_skin()`, LOD0 is bound to LimbNode joints with a skin.
    pub skeleton: Option<&'a Skeleton>,
    /// Joint animations (glTF semantics); written as AnimationStack/Layer/CurveNode/Curve.
    pub animations: &'a [Animation],
}

/// Write the scene to `path` as binary FBX 7.4.
pub fn write(path: &Path, scene: &FbxScene) -> Result<()> {
    let bytes = encode(scene)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Encode the scene as binary FBX 7.4 bytes.
pub fn encode(scene: &FbxScene) -> Result<Vec<u8>> {
    if scene.lods.is_empty() {
        bail!("FBX export needs at least one LOD mesh");
    }
    let mut b = Builder::new(scene.scale);
    b.build(scene);
    Ok(b.serialize())
}

// --------------------------------------------------------------------------------------------
// Node tree
// --------------------------------------------------------------------------------------------

const FBX_VERSION: u32 = 7400;
/// FBX time unit: 1/46186158000 of a second.
const KTIME_SECOND: f64 = 46_186_158_000.0;
const FIRST_ID: i64 = 1_000_000;

/// A binary FBX property.
#[derive(Clone, Debug)]
#[allow(dead_code)]
enum P {
    I16(i16),
    Bool(bool),
    I32(i32),
    F32(f32),
    F64(f64),
    I64(i64),
    ArrF32(Vec<f32>),
    ArrF64(Vec<f64>),
    ArrI64(Vec<i64>),
    ArrI32(Vec<i32>),
    ArrBool(Vec<bool>),
    Str(String),
    Raw(Vec<u8>),
}

impl From<&str> for P {
    fn from(s: &str) -> Self {
        P::Str(s.to_string())
    }
}
impl From<String> for P {
    fn from(s: String) -> Self {
        P::Str(s)
    }
}
impl From<i32> for P {
    fn from(v: i32) -> Self {
        P::I32(v)
    }
}
impl From<i64> for P {
    fn from(v: i64) -> Self {
        P::I64(v)
    }
}
impl From<f64> for P {
    fn from(v: f64) -> Self {
        P::F64(v)
    }
}
impl From<bool> for P {
    fn from(v: bool) -> Self {
        P::Bool(v)
    }
}

#[derive(Clone, Debug, Default)]
struct Node {
    name: String,
    props: Vec<P>,
    children: Vec<Node>,
}

impl Node {
    fn new(name: &str) -> Self {
        Node { name: name.to_string(), props: Vec::new(), children: Vec::new() }
    }
    fn with(name: &str, props: Vec<P>) -> Self {
        Node { name: name.to_string(), props, children: Vec::new() }
    }
    fn p(mut self, prop: impl Into<P>) -> Self {
        self.props.push(prop.into());
        self
    }
    fn add(&mut self, child: Node) -> &mut Self {
        self.children.push(child);
        self
    }
    fn child(mut self, child: Node) -> Self {
        self.children.push(child);
        self
    }
}

/// `Name::Class` object name in binary form (`name\0\x01Class`).
fn obj_name(name: &str, class: &str) -> String {
    format!("{name}\0\u{1}{class}")
}

/// A `Properties70` entry: `P: "name", "type", "label", "flags", values...`.
fn p70(name: &str, ty: &str, label: &str, flags: &str, values: Vec<P>) -> Node {
    let mut props: Vec<P> = vec![name.into(), ty.into(), label.into(), flags.into()];
    props.extend(values);
    Node::with("P", props)
}
fn p70_vec3(name: &str, ty: &str, label: &str, flags: &str, v: [f64; 3]) -> Node {
    p70(name, ty, label, flags, vec![v[0].into(), v[1].into(), v[2].into()])
}
fn p70_int(name: &str, v: i32) -> Node {
    p70(name, "int", "Integer", "", vec![v.into()])
}
fn p70_enum(name: &str, v: i32) -> Node {
    p70(name, "enum", "", "", vec![v.into()])
}
fn p70_bool(name: &str, v: bool) -> Node {
    p70(name, "bool", "", "", vec![P::I32(v as i32)])
}
fn p70_double(name: &str, v: f64) -> Node {
    p70(name, "double", "Number", "", vec![v.into()])
}
fn p70_number(name: &str, v: f64) -> Node {
    p70(name, "Number", "", "A", vec![v.into()])
}
fn p70_color(name: &str, c: [f64; 3]) -> Node {
    p70_vec3(name, "Color", "", "A", c)
}
fn p70_vector(name: &str, v: [f64; 3]) -> Node {
    p70_vec3(name, "Vector3D", "Vector", "", v)
}
fn p70_string(name: &str, v: &str) -> Node {
    p70(name, "KString", "", "", vec![v.into()])
}
fn p70_ktime(name: &str, v: i64) -> Node {
    p70(name, "KTime", "Time", "", vec![v.into()])
}

/// Local transform block shared by all Model nodes.
fn lcl_props(t: [f64; 3], r: [f64; 3], s: [f64; 3]) -> Vec<Node> {
    vec![
        p70_vec3("Lcl Translation", "Lcl Translation", "", "A", t),
        p70_vec3("Lcl Rotation", "Lcl Rotation", "", "A", r),
        p70_vec3("Lcl Scaling", "Lcl Scaling", "", "A", s),
    ]
}

// --------------------------------------------------------------------------------------------
// Scene builder
// --------------------------------------------------------------------------------------------

struct Builder {
    next_id: i64,
    scale: f32,
    objects: Vec<Node>,
    /// (child, parent, property) — property `Some` => OP connection.
    connections: Vec<(i64, i64, Option<String>)>,
    counts: Vec<(&'static str, usize)>,
    /// Longest animation (seconds), for the global time span.
    anim_end: f64,
}

impl Builder {
    fn new(scale: f32) -> Self {
        Self { next_id: FIRST_ID, scale, objects: Vec::new(), connections: Vec::new(), counts: Vec::new(), anim_end: 0.0 }
    }

    fn id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn count(&mut self, ty: &'static str) {
        if let Some(c) = self.counts.iter_mut().find(|(t, _)| *t == ty) {
            c.1 += 1;
        } else {
            self.counts.push((ty, 1));
        }
    }

    fn push(&mut self, ty: &'static str, node: Node) {
        self.count(ty);
        self.objects.push(node);
    }

    fn oo(&mut self, child: i64, parent: i64) {
        self.connections.push((child, parent, None));
    }
    fn op(&mut self, child: i64, parent: i64, prop: &str) {
        self.connections.push((child, parent, Some(prop.to_string())));
    }

    // ---- objects ----

    fn build(&mut self, scene: &FbxScene) {
        let material_id = self.material(&scene.material);

        // LOD models.
        let mut lod0_model = 0;
        if scene.lods.len() > 1 {
            let group_id = self.lod_group(&scene.name, scene.lods.len());
            for (i, lod) in scene.lods.iter().enumerate() {
                let model = self.mesh_model(&lod.name, lod.mesh, Some(material_id), group_id);
                if i == 0 {
                    lod0_model = model;
                }
            }
        } else {
            lod0_model = self.mesh_model(&scene.lods[0].name, scene.lods[0].mesh, Some(material_id), 0);
        }
        let lod0_geometry = lod0_model + 1; // geometry id is allocated right after its model

        for (name, mesh) in &scene.collision {
            self.mesh_model(name, mesh, None, 0);
        }

        // Skin + animation.
        if let Some(skel) = scene.skeleton {
            let lod0 = scene.lods[0].mesh;
            if !skel.joints.is_empty() {
                let joint_models = self.skeleton(skel);
                if lod0.has_skin() {
                    self.skin(&scene.name, lod0, skel, lod0_model, lod0_geometry, &joint_models);
                }
                for anim in scene.animations {
                    self.animation(anim, skel, &joint_models);
                }
            }
        }
    }

    fn material(&mut self, m: &FbxMaterial) -> i64 {
        let id = self.id();
        let c = m.base_color;
        let rough = m.roughness.clamp(0.0, 1.0) as f64;
        let shininess = ((1.0 - rough) * (1.0 - rough) * 100.0).max(1.0);
        let spec = 0.04 + 0.96 * m.metallic.clamp(0.0, 1.0) as f64;
        let mut props = Node::new("Properties70");
        props.add(p70_string("ShadingModel", "phong"));
        props.add(p70_color("EmissiveColor", [0.0, 0.0, 0.0]));
        props.add(p70_number("EmissiveFactor", 0.0));
        props.add(p70_color("AmbientColor", [0.0, 0.0, 0.0]));
        props.add(p70_number("AmbientFactor", 1.0));
        props.add(p70_color("DiffuseColor", [c[0] as f64, c[1] as f64, c[2] as f64]));
        props.add(p70_number("DiffuseFactor", 1.0));
        props.add(p70_color("TransparentColor", [1.0, 1.0, 1.0]));
        props.add(p70_number("TransparencyFactor", (1.0 - c[3] as f64).clamp(0.0, 1.0)));
        props.add(p70_number("Opacity", (c[3] as f64).clamp(0.0, 1.0)));
        props.add(p70_vector("NormalMap", [0.0, 0.0, 0.0]));
        props.add(p70_double("BumpFactor", 1.0));
        props.add(p70_color("SpecularColor", [spec, spec, spec]));
        props.add(p70_number("SpecularFactor", 1.0));
        props.add(p70_number("ShininessExponent", shininess));
        props.add(p70_number("Shininess", shininess));
        props.add(p70_color("ReflectionColor", [1.0, 1.0, 1.0]));
        props.add(p70_number("ReflectionFactor", m.metallic.clamp(0.0, 1.0) as f64));
        // Legacy (pre-7.x) mirror fields some importers still read.
        props.add(p70_vector("Diffuse", [c[0] as f64, c[1] as f64, c[2] as f64]));
        props.add(p70_vector("Specular", [spec, spec, spec]));
        props.add(p70_vector("Emissive", [0.0, 0.0, 0.0]));
        props.add(p70_vector("Ambient", [0.0, 0.0, 0.0]));

        let node = Node::new("Material")
            .p(id)
            .p(obj_name(&m.name, "Material"))
            .p("")
            .child(Node::with("Version", vec![102.into()]))
            .child(Node::with("ShadingModel", vec!["phong".into()]))
            .child(Node::with("MultiLayer", vec![0.into()]))
            .child(props);
        self.push("Material", node);

        let textures: [(&Option<String>, &str, &str); 4] = [
            (&m.albedo_file, "albedo", "DiffuseColor"),
            (&m.normal_file, "normal", "NormalMap"),
            (&m.ao_file, "ao", "AmbientFactor"),
            (&m.orm_file, "orm", "ReflectionFactor"),
        ];
        for (file, suffix, prop) in textures {
            if let Some(file) = file {
                let tex = self.texture(&format!("{}_{suffix}", m.name), file);
                self.op(tex, id, prop);
            }
        }
        id
    }

    fn texture(&mut self, name: &str, file: &str) -> i64 {
        let file = Path::new(file).file_name().and_then(|f| f.to_str()).unwrap_or(file).to_string();
        let video_id = self.id();
        let video = Node::new("Video")
            .p(video_id)
            .p(obj_name(name, "Video"))
            .p("Clip")
            .child(Node::with("Type", vec!["Clip".into()]))
            .child(Node::new("Properties70").child(p70("Path", "KString", "XRefUrl", "", vec![file.as_str().into()])))
            .child(Node::with("UseMipMap", vec![0.into()]))
            .child(Node::with("Filename", vec![file.as_str().into()]))
            .child(Node::with("RelativeFilename", vec![file.as_str().into()]));
        self.push("Video", video);

        let tex_id = self.id();
        let tex = Node::new("Texture")
            .p(tex_id)
            .p(obj_name(name, "Texture"))
            .p("")
            .child(Node::with("Type", vec!["TextureVideoClip".into()]))
            .child(Node::with("Version", vec![202.into()]))
            .child(Node::with("TextureName", vec![obj_name(name, "Texture").into()]))
            .child(
                Node::new("Properties70")
                    .child(p70_enum("CurrentTextureBlendMode", 0))
                    .child(p70_string("UVSet", "UVMap"))
                    .child(p70_bool("UseMaterial", true)),
            )
            .child(Node::with("Media", vec![obj_name(name, "Video").into()]))
            .child(Node::with("FileName", vec![file.as_str().into()]))
            .child(Node::with("RelativeFilename", vec![file.as_str().into()]))
            .child(Node::with("ModelUVTranslation", vec![0.0.into(), 0.0.into()]))
            .child(Node::with("ModelUVScaling", vec![1.0.into(), 1.0.into()]))
            .child(Node::with("Texture_Alpha_Source", vec!["None".into()]))
            .child(Node::with("Cropping", vec![0.into(), 0.into(), 0.into(), 0.into()]));
        self.push("Texture", tex);
        self.oo(video_id, tex_id);
        tex_id
    }

    /// A Model of class `LodGroup` with a LodGroup NodeAttribute; returns the model id.
    fn lod_group(&mut self, name: &str, levels: usize) -> i64 {
        let model_id = self.id();
        let mut props = Node::new("Properties70");
        for n in lcl_props([0.0; 3], [0.0; 3], [1.0; 3]) {
            props.add(n);
        }
        props.add(p70_int("DefaultAttributeIndex", 0));
        props.add(p70_enum("InheritType", 1));
        let model = Node::new("Model")
            .p(model_id)
            .p(obj_name(name, "Model"))
            .p("LodGroup")
            .child(Node::with("Version", vec![232.into()]))
            .child(props)
            .child(Node::with("Shading", vec![P::Bool(true)]))
            .child(Node::with("Culling", vec!["CullingOff".into()]));
        self.push("Model", model);
        self.oo(model_id, 0);

        let attr_id = self.id();
        let mut props = Node::new("Properties70");
        props.add(p70_bool("MinMaxDistance", false));
        props.add(p70_double("MinDistance", -100.0));
        props.add(p70_double("MaxDistance", 100.0));
        props.add(p70_bool("WorldSpace", false));
        props.add(p70_bool("ThresholdsUsedAsPercentage", true));
        // Level i (i >= 1) is shown while the object covers less than this percentage of
        // the screen: 50%, 25%, 12.5%...
        let mut pct = 50.0;
        for i in 0..levels.saturating_sub(1) {
            props.add(p70(&format!("Thresholds|Level{i}"), "Distance", "", "", vec![pct.into(), "cm".into()]));
            pct *= 0.5;
        }
        for i in 0..levels {
            props.add(p70_enum(&format!("DisplayLevels|Level{i}"), 0));
        }
        let attr = Node::new("NodeAttribute")
            .p(attr_id)
            .p(obj_name(name, "NodeAttribute"))
            .p("LodGroup")
            .child(props)
            .child(Node::with("TypeFlags", vec!["LodGroup".into()]));
        self.push("NodeAttribute", attr);
        self.oo(attr_id, model_id);
        model_id
    }

    /// Model + Geometry pair; the geometry id is always `model_id + 1`.
    fn mesh_model(&mut self, name: &str, mesh: &Mesh, material: Option<i64>, parent: i64) -> i64 {
        let model_id = self.id();
        let geom_id = self.id();
        let mut props = Node::new("Properties70");
        for n in lcl_props([0.0; 3], [0.0; 3], [1.0; 3]) {
            props.add(n);
        }
        props.add(p70_int("DefaultAttributeIndex", 0));
        props.add(p70_enum("InheritType", 1));
        let model = Node::new("Model")
            .p(model_id)
            .p(obj_name(name, "Model"))
            .p("Mesh")
            .child(Node::with("Version", vec![232.into()]))
            .child(props)
            .child(Node::with("Shading", vec![P::Bool(true)]))
            .child(Node::with("Culling", vec!["CullingOff".into()]));
        self.push("Model", model);
        self.oo(model_id, parent);

        let geom = geometry_node(geom_id, name, mesh, self.scale, material.is_some());
        self.push("Geometry", geom);
        self.oo(geom_id, model_id);
        if let Some(m) = material {
            self.oo(m, model_id);
        }
        model_id
    }

    /// LimbNode models for every joint (connected to their parents); returns model ids.
    fn skeleton(&mut self, skel: &Skeleton) -> Vec<i64> {
        let ids: Vec<i64> = skel.joints.iter().map(|_| 0).collect();
        let mut ids = ids;
        // Allocate ids first so that children can be connected to parents in any order.
        for id in ids.iter_mut() {
            *id = self.id();
        }
        for (ji, joint) in skel.joints.iter().enumerate() {
            let (t, r, s) = decompose(&joint.local, self.scale);
            let mut props = Node::new("Properties70");
            for n in lcl_props(t, r, s) {
                props.add(n);
            }
            props.add(p70_int("DefaultAttributeIndex", 0));
            props.add(p70_enum("InheritType", 1));
            let model = Node::new("Model")
                .p(ids[ji])
                .p(obj_name(&joint.name, "Model"))
                .p("LimbNode")
                .child(Node::with("Version", vec![232.into()]))
                .child(props)
                .child(Node::with("Shading", vec![P::Bool(true)]))
                .child(Node::with("Culling", vec!["CullingOff".into()]));
            self.push("Model", model);
            let parent = joint.parent.and_then(|p| ids.get(p).copied()).unwrap_or(0);
            self.oo(ids[ji], parent);

            let attr_id = self.id();
            let attr = Node::new("NodeAttribute")
                .p(attr_id)
                .p(obj_name(&joint.name, "NodeAttribute"))
                .p("LimbNode")
                .child(Node::new("Properties70").child(p70_double("Size", 1.0)))
                .child(Node::with("TypeFlags", vec!["Skeleton".into()]));
            self.push("NodeAttribute", attr);
            self.oo(attr_id, ids[ji]);
        }
        ids
    }

    /// Skin deformer with one cluster per joint that influences at least one vertex, plus a bind pose.
    fn skin(&mut self, name: &str, mesh: &Mesh, skel: &Skeleton, mesh_model: i64, geometry: i64, joint_models: &[i64]) {
        let skin_id = self.id();
        let skin = Node::new("Deformer")
            .p(skin_id)
            .p(obj_name(name, "Deformer"))
            .p("Skin")
            .child(Node::with("Version", vec![101.into()]))
            .child(Node::with("Link_DeformAcuracy", vec![50.0.into()]))
            .child(Node::with("SkinningType", vec!["Linear".into()]));
        self.push("Deformer", skin);
        self.oo(skin_id, geometry);

        // Gather per-joint influences.
        let nj = skel.joints.len();
        let mut per_joint: Vec<(Vec<i32>, Vec<f64>)> = vec![(Vec::new(), Vec::new()); nj];
        for (vi, (j, w)) in mesh.joints.iter().zip(mesh.weights.iter()).enumerate() {
            for k in 0..4 {
                let joint = j[k] as usize;
                if w[k] > 0.0 && joint < nj {
                    per_joint[joint].0.push(vi as i32);
                    per_joint[joint].1.push(w[k] as f64);
                }
            }
        }

        let mut pose = Node::new("Pose");
        let pose_id = self.id();
        let mut pose_nodes: Vec<Node> = vec![Node::new("PoseNode")
            .child(Node::with("Node", vec![mesh_model.into()]))
            .child(Node::with("Matrix", vec![P::ArrF64(Mat4::IDENTITY.to_cols_array().iter().map(|v| *v as f64).collect())]))];

        for (ji, joint) in skel.joints.iter().enumerate() {
            let inv_bind = scaled_matrix(&joint.inverse_bind, self.scale);
            let bind = inv_bind.inverse();
            pose_nodes.push(
                Node::new("PoseNode")
                    .child(Node::with("Node", vec![joint_models[ji].into()]))
                    .child(Node::with("Matrix", vec![P::ArrF64(mat_f64(&bind))])),
            );
            let (idx, wts) = &per_joint[ji];
            if idx.is_empty() {
                continue;
            }
            let cluster_id = self.id();
            let cluster = Node::new("Deformer")
                .p(cluster_id)
                .p(obj_name(&format!("{name}_{}", joint.name), "SubDeformer"))
                .p("Cluster")
                .child(Node::with("Version", vec![100.into()]))
                .child(Node::with("UserData", vec!["".into(), "".into()]))
                .child(Node::with("Indexes", vec![P::ArrI32(idx.clone())]))
                .child(Node::with("Weights", vec![P::ArrF64(wts.clone())]))
                .child(Node::with("Transform", vec![P::ArrF64(mat_f64(&inv_bind))]))
                .child(Node::with("TransformLink", vec![P::ArrF64(mat_f64(&bind))]));
            self.push("Deformer", cluster);
            self.oo(cluster_id, skin_id);
            self.oo(joint_models[ji], cluster_id);
        }

        pose = pose
            .p(pose_id)
            .p(obj_name(&format!("{name}_BindPose"), "Pose"))
            .p("BindPose")
            .child(Node::with("Type", vec!["BindPose".into()]))
            .child(Node::with("Version", vec![100.into()]))
            .child(Node::with("NbPoseNodes", vec![(pose_nodes.len() as i32).into()]));
        for n in pose_nodes {
            pose.add(n);
        }
        self.push("Pose", pose);
    }

    /// One AnimationStack + BaseLayer with a CurveNode per (joint, property) and 3 curves each.
    fn animation(&mut self, anim: &Animation, skel: &Skeleton, joint_models: &[i64]) {
        let mut end = 0.0f64;
        for ch in &anim.channels {
            if let Some(t) = ch.times.last() {
                end = end.max(*t as f64);
            }
        }
        self.anim_end = self.anim_end.max(end);
        let end_k = (end * KTIME_SECOND).round() as i64;
        let name = if anim.name.is_empty() { "Take 001".to_string() } else { anim.name.clone() };

        let stack_id = self.id();
        let stack = Node::new("AnimationStack")
            .p(stack_id)
            .p(obj_name(&name, "AnimStack"))
            .p("")
            .child(
                Node::new("Properties70")
                    .child(p70_ktime("LocalStart", 0))
                    .child(p70_ktime("LocalStop", end_k))
                    .child(p70_ktime("ReferenceStart", 0))
                    .child(p70_ktime("ReferenceStop", end_k)),
            );
        self.push("AnimationStack", stack);

        let layer_id = self.id();
        let layer = Node::new("AnimationLayer").p(layer_id).p(obj_name("BaseLayer", "AnimLayer")).p("");
        self.push("AnimationLayer", layer);
        self.oo(layer_id, stack_id);

        for ch in &anim.channels {
            if ch.joint >= skel.joints.len() || ch.times.is_empty() {
                continue;
            }
            let joint = &skel.joints[ch.joint];
            let (rest_t, rest_r, rest_s) = decompose(&joint.local, self.scale);
            let cubic = ch.interpolation == "CUBICSPLINE";
            let stride = |comps: usize| if cubic { comps * 3 } else { comps };
            let pick = |key: usize, comps: usize, c: usize| -> f32 {
                let base = key * stride(comps) + if cubic { comps } else { 0 };
                ch.values.get(base + c).copied().unwrap_or(0.0)
            };
            let nkeys = ch.times.len();
            let (prop, short, default, keys): (&str, &str, [f64; 3], Vec<[f64; 3]>) = match ch.path.as_str() {
                "translation" => {
                    let s = self.scale as f64;
                    let k = (0..nkeys).map(|i| [pick(i, 3, 0) as f64 * s, pick(i, 3, 1) as f64 * s, pick(i, 3, 2) as f64 * s]).collect();
                    ("Lcl Translation", "T", rest_t, k)
                }
                "scale" => {
                    let k = (0..nkeys).map(|i| [pick(i, 3, 0) as f64, pick(i, 3, 1) as f64, pick(i, 3, 2) as f64]).collect();
                    ("Lcl Scaling", "S", rest_s, k)
                }
                "rotation" => {
                    let mut k: Vec<[f64; 3]> = Vec::with_capacity(nkeys);
                    for i in 0..nkeys {
                        let q = Quat::from_xyzw(pick(i, 4, 0), pick(i, 4, 1), pick(i, 4, 2), pick(i, 4, 3)).normalize();
                        let mut e = quat_to_euler_deg(q);
                        if let Some(prev) = k.last() {
                            for c in 0..3 {
                                while e[c] - prev[c] > 180.0 {
                                    e[c] -= 360.0;
                                }
                                while e[c] - prev[c] < -180.0 {
                                    e[c] += 360.0;
                                }
                            }
                        }
                        k.push(e);
                    }
                    ("Lcl Rotation", "R", rest_r, k)
                }
                _ => continue,
            };

            let cn_id = self.id();
            let cn = Node::new("AnimationCurveNode")
                .p(cn_id)
                .p(obj_name(short, "AnimCurveNode"))
                .p("")
                .child(
                    Node::new("Properties70")
                        .child(p70_number("d|X", default[0]))
                        .child(p70_number("d|Y", default[1]))
                        .child(p70_number("d|Z", default[2])),
                );
            self.push("AnimationCurveNode", cn);
            self.oo(cn_id, layer_id);
            self.op(cn_id, joint_models[ch.joint], prop);

            let times: Vec<i64> = ch.times.iter().map(|t| (*t as f64 * KTIME_SECOND).round() as i64).collect();
            // 0x2 = constant, 0x4 = linear (cubic spline tangents are dropped: keys become linear).
            let flags: i32 = if ch.interpolation == "STEP" { 0x2 } else { 0x4 };
            for (c, axis) in ["d|X", "d|Y", "d|Z"].iter().enumerate() {
                let curve_id = self.id();
                let values: Vec<f32> = keys.iter().map(|k| k[c] as f32).collect();
                let curve = Node::new("AnimationCurve")
                    .p(curve_id)
                    .p(obj_name("", "AnimCurve"))
                    .p("")
                    .child(Node::with("Default", vec![default[c].into()]))
                    .child(Node::with("KeyVer", vec![4008.into()]))
                    .child(Node::with("KeyTime", vec![P::ArrI64(times.clone())]))
                    .child(Node::with("KeyValueFloat", vec![P::ArrF32(values)]))
                    .child(Node::with("KeyAttrFlags", vec![P::ArrI32(vec![flags])]))
                    .child(Node::with("KeyAttrDataFloat", vec![P::ArrF32(vec![0.0, 0.0, 0.0, 0.0])]))
                    .child(Node::with("KeyAttrRefCount", vec![P::ArrI32(vec![nkeys as i32])]));
                self.push("AnimationCurve", curve);
                self.op(curve_id, cn_id, axis);
            }
        }
    }

    // ---- top-level document ----

    fn serialize(self) -> Vec<u8> {
        let mut top: Vec<Node> = Vec::new();
        top.push(header_extension());
        top.push(Node::with("FileId", vec![P::Raw(file_id(&self.objects))]));
        top.push(Node::with("CreationTime", vec![creation_time().into()]));
        top.push(Node::with("Creator", vec![creator().into()]));
        top.push(global_settings(self.anim_end));
        top.push(documents());
        top.push(Node::new("References"));
        top.push(definitions(&self.counts));

        let mut objects = Node::new("Objects");
        objects.children = self.objects;
        top.push(objects);

        let mut connections = Node::new("Connections");
        for (child, parent, prop) in &self.connections {
            let mut c = Node::new("C");
            if let Some(p) = prop {
                c = c.p("OP").p(*child).p(*parent).p(p.as_str());
            } else {
                c = c.p("OO").p(*child).p(*parent);
            }
            connections.add(c);
        }
        top.push(connections);

        top.push(Node::new("Takes").child(Node::with("Current", vec!["".into()])));

        let mut out = Vec::with_capacity(1 << 16);
        out.extend_from_slice(b"Kaydara FBX Binary  \0\x1a\0");
        out.extend_from_slice(&FBX_VERSION.to_le_bytes());
        for node in &top {
            write_node(&mut out, node);
        }
        out.extend_from_slice(&[0u8; 13]);
        write_footer(&mut out);
        out
    }
}

// --------------------------------------------------------------------------------------------
// Geometry
// --------------------------------------------------------------------------------------------

/// Flat polygon-vertex index list and the size of each polygon.
fn polygon_vertices(mesh: &Mesh) -> (Vec<u32>, Vec<u8>) {
    if mesh.has_polygons() {
        let mut pv = Vec::with_capacity(mesh.polygons.len() * 4);
        let mut sizes = Vec::with_capacity(mesh.polygons.len());
        for p in &mesh.polygons {
            pv.extend_from_slice(&p[..3]);
            if p[3] != NO_VERTEX {
                pv.push(p[3]);
                sizes.push(4);
            } else {
                sizes.push(3);
            }
        }
        (pv, sizes)
    } else {
        (mesh.indices.clone(), vec![3; mesh.triangle_count()])
    }
}

fn geometry_node(id: i64, name: &str, mesh: &Mesh, scale: f32, with_material: bool) -> Node {
    let (pv, sizes) = polygon_vertices(mesh);

    let mut vertices = Vec::with_capacity(mesh.positions.len() * 3);
    for p in &mesh.positions {
        vertices.extend_from_slice(&[(p.x * scale) as f64, (p.y * scale) as f64, (p.z * scale) as f64]);
    }

    let mut poly_index = Vec::with_capacity(pv.len());
    let mut cursor = 0usize;
    for &n in &sizes {
        let n = n as usize;
        for k in 0..n {
            let i = pv[cursor + k] as i32;
            poly_index.push(if k + 1 == n { !i } else { i });
        }
        cursor += n;
    }

    let mut geom = Node::new("Geometry")
        .p(id)
        .p(obj_name(name, "Geometry"))
        .p("Mesh")
        .child(Node::new("Properties70"))
        .child(Node::with("GeometryVersion", vec![124.into()]))
        .child(Node::with("Vertices", vec![P::ArrF64(vertices)]))
        .child(Node::with("PolygonVertexIndex", vec![P::ArrI32(poly_index)]));

    let mut layer = Node::new("Layer").p(0).child(Node::with("Version", vec![100.into()]));
    let layer_ref = |ty: &str| Node::new("LayerElement").child(Node::with("Type", vec![ty.into()])).child(Node::with("TypedIndex", vec![0.into()]));

    if mesh.has_normals() {
        let mut normals = Vec::with_capacity(pv.len() * 3);
        for &i in &pv {
            let n = mesh.normals[i as usize];
            normals.extend_from_slice(&[n.x as f64, n.y as f64, n.z as f64]);
        }
        geom.add(
            Node::new("LayerElementNormal")
                .p(0)
                .child(Node::with("Version", vec![101.into()]))
                .child(Node::with("Name", vec!["".into()]))
                .child(Node::with("MappingInformationType", vec!["ByPolygonVertex".into()]))
                .child(Node::with("ReferenceInformationType", vec!["Direct".into()]))
                .child(Node::with("Normals", vec![P::ArrF64(normals)])),
        );
        layer.add(layer_ref("LayerElementNormal"));
    }

    if mesh.has_uvs() {
        let mut uv = Vec::with_capacity(mesh.uvs.len() * 2);
        for t in &mesh.uvs {
            uv.extend_from_slice(&[t.x as f64, (1.0 - t.y) as f64]);
        }
        let uv_index: Vec<i32> = pv.iter().map(|&i| i as i32).collect();
        geom.add(
            Node::new("LayerElementUV")
                .p(0)
                .child(Node::with("Version", vec![101.into()]))
                .child(Node::with("Name", vec!["UVMap".into()]))
                .child(Node::with("MappingInformationType", vec!["ByPolygonVertex".into()]))
                .child(Node::with("ReferenceInformationType", vec!["IndexToDirect".into()]))
                .child(Node::with("UV", vec![P::ArrF64(uv)]))
                .child(Node::with("UVIndex", vec![P::ArrI32(uv_index)])),
        );
        layer.add(layer_ref("LayerElementUV"));
    }

    if mesh.has_colors() {
        let mut colors = Vec::with_capacity(pv.len() * 4);
        for &i in &pv {
            let c = mesh.colors[i as usize];
            colors.extend_from_slice(&[c[0] as f64, c[1] as f64, c[2] as f64, c[3] as f64]);
        }
        geom.add(
            Node::new("LayerElementColor")
                .p(0)
                .child(Node::with("Version", vec![101.into()]))
                .child(Node::with("Name", vec!["Col".into()]))
                .child(Node::with("MappingInformationType", vec!["ByPolygonVertex".into()]))
                .child(Node::with("ReferenceInformationType", vec!["Direct".into()]))
                .child(Node::with("Colors", vec![P::ArrF64(colors)])),
        );
        layer.add(layer_ref("LayerElementColor"));
    }

    if with_material {
        geom.add(
            Node::new("LayerElementMaterial")
                .p(0)
                .child(Node::with("Version", vec![101.into()]))
                .child(Node::with("Name", vec!["".into()]))
                .child(Node::with("MappingInformationType", vec!["AllSame".into()]))
                .child(Node::with("ReferenceInformationType", vec!["IndexToDirect".into()]))
                .child(Node::with("Materials", vec![P::ArrI32(vec![0])])),
        );
        layer.add(layer_ref("LayerElementMaterial"));
    }

    geom.add(layer);
    geom
}

// --------------------------------------------------------------------------------------------
// Transforms
// --------------------------------------------------------------------------------------------

fn scaled_matrix(m: &[[f32; 4]; 4], scale: f32) -> Mat4 {
    let mut m = Mat4::from_cols_array_2d(m);
    m.w_axis.x *= scale;
    m.w_axis.y *= scale;
    m.w_axis.z *= scale;
    m
}

fn mat_f64(m: &Mat4) -> Vec<f64> {
    m.to_cols_array().iter().map(|v| *v as f64).collect()
}

/// FBX `Lcl Rotation` uses Euler XYZ (X applied first, i.e. R = Rz * Ry * Rx), in degrees.
fn quat_to_euler_deg(q: Quat) -> [f64; 3] {
    let (z, y, x) = q.to_euler(EulerRot::ZYX);
    [x.to_degrees() as f64, y.to_degrees() as f64, z.to_degrees() as f64]
}

/// Translation (scaled), Euler rotation in degrees and scale of a column-major local matrix.
fn decompose(local: &[[f32; 4]; 4], scale: f32) -> ([f64; 3], [f64; 3], [f64; 3]) {
    let m = scaled_matrix(local, scale);
    let (s, r, t) = m.to_scale_rotation_translation();
    let s = if s.is_finite() && s != Vec3::ZERO { s } else { Vec3::ONE };
    (
        [t.x as f64, t.y as f64, t.z as f64],
        quat_to_euler_deg(r.normalize()),
        [s.x as f64, s.y as f64, s.z as f64],
    )
}

// --------------------------------------------------------------------------------------------
// Header / settings / definitions
// --------------------------------------------------------------------------------------------

fn creator() -> String {
    format!("Polysquish {}", crate::VERSION)
}

fn now_civil() -> (i32, i32, i32, i32, i32, i32, i32) {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let ms = d.subsec_millis() as i32;
    let days = (secs / 86400) as i64;
    let (h, mi, s) = (((secs % 86400) / 3600) as i32, ((secs % 3600) / 60) as i32, (secs % 60) as i32);
    // Howard Hinnant's civil-from-days.
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as i32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as i32;
    let year = if month <= 2 { y + 1 } else { y } as i32;
    (year, month, day, h, mi, s, ms)
}

fn creation_time() -> String {
    let (y, mo, d, h, mi, s, ms) = now_civil();
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}:{ms:03}")
}

fn header_extension() -> Node {
    let (y, mo, d, h, mi, s, ms) = now_civil();
    let stamp = Node::new("CreationTimeStamp")
        .child(Node::with("Version", vec![1000.into()]))
        .child(Node::with("Year", vec![y.into()]))
        .child(Node::with("Month", vec![mo.into()]))
        .child(Node::with("Day", vec![d.into()]))
        .child(Node::with("Hour", vec![h.into()]))
        .child(Node::with("Minute", vec![mi.into()]))
        .child(Node::with("Second", vec![s.into()]))
        .child(Node::with("Millisecond", vec![ms.into()]));
    let meta = Node::new("MetaData")
        .child(Node::with("Version", vec![100.into()]))
        .child(Node::with("Title", vec!["".into()]))
        .child(Node::with("Subject", vec!["".into()]))
        .child(Node::with("Author", vec!["".into()]))
        .child(Node::with("Keywords", vec!["".into()]))
        .child(Node::with("Revision", vec!["".into()]))
        .child(Node::with("Comment", vec!["".into()]));
    let scene_info = Node::new("SceneInfo")
        .p(obj_name("GlobalInfo", "SceneInfo"))
        .p("UserData")
        .child(Node::with("Type", vec!["UserData".into()]))
        .child(Node::with("Version", vec![100.into()]))
        .child(meta)
        .child(
            Node::new("Properties70")
                .child(p70("DocumentUrl", "KString", "Url", "", vec!["".into()]))
                .child(p70("SrcDocumentUrl", "KString", "Url", "", vec!["".into()]))
                .child(p70("Original", "Compound", "", "", vec![]))
                .child(p70("Original|ApplicationVendor", "KString", "", "", vec!["Polysquish".into()]))
                .child(p70("Original|ApplicationName", "KString", "", "", vec!["Polysquish".into()]))
                .child(p70("Original|ApplicationVersion", "KString", "", "", vec![crate::VERSION.into()]))
                .child(p70("Original|DateTime_GMT", "DateTime", "", "", vec![creation_time().into()]))
                .child(p70("Original|FileName", "KString", "", "", vec!["".into()]))
                .child(p70("LastSaved", "Compound", "", "", vec![]))
                .child(p70("LastSaved|ApplicationVendor", "KString", "", "", vec!["Polysquish".into()]))
                .child(p70("LastSaved|ApplicationName", "KString", "", "", vec!["Polysquish".into()]))
                .child(p70("LastSaved|ApplicationVersion", "KString", "", "", vec![crate::VERSION.into()]))
                .child(p70("LastSaved|DateTime_GMT", "DateTime", "", "", vec![creation_time().into()])),
        );
    Node::new("FBXHeaderExtension")
        .child(Node::with("FBXHeaderVersion", vec![1003.into()]))
        .child(Node::with("FBXVersion", vec![(FBX_VERSION as i32).into()]))
        .child(Node::with("EncryptionType", vec![0.into()]))
        .child(stamp)
        .child(Node::with("Creator", vec![creator().into()]))
        .child(scene_info)
}

fn file_id(objects: &[Node]) -> Vec<u8> {
    // Any 16 bytes; derive something stable from the object names.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for n in objects {
        for p in &n.props {
            if let P::Str(s) = p {
                for b in s.bytes() {
                    h ^= b as u64;
                    h = h.wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
        }
    }
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&h.rotate_left(29).to_le_bytes());
    out
}

fn global_settings(anim_end: f64) -> Node {
    let stop = ((anim_end.max(1.0)) * KTIME_SECOND).round() as i64;
    Node::new("GlobalSettings").child(Node::with("Version", vec![1000.into()])).child(
        Node::new("Properties70")
            .child(p70_int("UpAxis", 1))
            .child(p70_int("UpAxisSign", 1))
            .child(p70_int("FrontAxis", 2))
            .child(p70_int("FrontAxisSign", 1))
            .child(p70_int("CoordAxis", 0))
            .child(p70_int("CoordAxisSign", 1))
            .child(p70_int("OriginalUpAxis", 1))
            .child(p70_int("OriginalUpAxisSign", 1))
            .child(p70_double("UnitScaleFactor", 1.0))
            .child(p70_double("OriginalUnitScaleFactor", 1.0))
            .child(p70_vec3("AmbientColor", "ColorRGB", "Color", "", [0.0, 0.0, 0.0]))
            .child(p70_string("DefaultCamera", "Producer Perspective"))
            .child(p70_enum("TimeMode", 11))
            .child(p70_enum("TimeProtocol", 2))
            .child(p70_enum("SnapOnFrameMode", 0))
            .child(p70_ktime("TimeSpanStart", 0))
            .child(p70_ktime("TimeSpanStop", stop))
            .child(p70_double("CustomFrameRate", -1.0))
            .child(p70("TimeMarker", "Compound", "", "", vec![]))
            .child(p70_int("CurrentTimeMarker", -1)),
    )
}

fn documents() -> Node {
    Node::new("Documents").child(Node::with("Count", vec![1.into()])).child(
        Node::new("Document")
            .p(FIRST_ID - 1)
            .p("")
            .p("Scene")
            .child(
                Node::new("Properties70")
                    .child(p70("SourceObject", "object", "", "", vec![]))
                    .child(p70_string("ActiveAnimStackName", "")),
            )
            .child(Node::with("RootNode", vec![0i64.into()])),
    )
}

fn template_fbx_node() -> Node {
    let mut p = Node::new("Properties70");
    p.add(p70_enum("QuaternionInterpolate", 0));
    for n in ["RotationOffset", "RotationPivot", "ScalingOffset", "ScalingPivot"] {
        p.add(p70_vector(n, [0.0; 3]));
    }
    p.add(p70_bool("TranslationActive", false));
    p.add(p70_vector("TranslationMin", [0.0; 3]));
    p.add(p70_vector("TranslationMax", [0.0; 3]));
    for n in ["TranslationMinX", "TranslationMinY", "TranslationMinZ", "TranslationMaxX", "TranslationMaxY", "TranslationMaxZ"] {
        p.add(p70_bool(n, false));
    }
    p.add(p70_enum("RotationOrder", 0));
    p.add(p70_bool("RotationSpaceForLimitOnly", false));
    for n in ["RotationStiffnessX", "RotationStiffnessY", "RotationStiffnessZ"] {
        p.add(p70_double(n, 0.0));
    }
    p.add(p70_double("AxisLen", 10.0));
    p.add(p70_vector("PreRotation", [0.0; 3]));
    p.add(p70_vector("PostRotation", [0.0; 3]));
    p.add(p70_bool("RotationActive", false));
    p.add(p70_vector("RotationMin", [0.0; 3]));
    p.add(p70_vector("RotationMax", [0.0; 3]));
    for n in ["RotationMinX", "RotationMinY", "RotationMinZ", "RotationMaxX", "RotationMaxY", "RotationMaxZ"] {
        p.add(p70_bool(n, false));
    }
    p.add(p70_enum("InheritType", 0));
    p.add(p70_bool("ScalingActive", false));
    p.add(p70_vector("ScalingMin", [0.0; 3]));
    p.add(p70_vector("ScalingMax", [1.0; 3]));
    for n in ["ScalingMinX", "ScalingMinY", "ScalingMinZ", "ScalingMaxX", "ScalingMaxY", "ScalingMaxZ"] {
        p.add(p70_bool(n, false));
    }
    p.add(p70_vector("GeometricTranslation", [0.0; 3]));
    p.add(p70_vector("GeometricRotation", [0.0; 3]));
    p.add(p70_vector("GeometricScaling", [1.0; 3]));
    for n in [
        "MinDampRangeX", "MinDampRangeY", "MinDampRangeZ", "MaxDampRangeX", "MaxDampRangeY", "MaxDampRangeZ", "MinDampStrengthX", "MinDampStrengthY",
        "MinDampStrengthZ", "MaxDampStrengthX", "MaxDampStrengthY", "MaxDampStrengthZ", "PreferedAngleX", "PreferedAngleY", "PreferedAngleZ",
    ] {
        p.add(p70_double(n, 0.0));
    }
    p.add(p70("LookAtProperty", "object", "", "", vec![]));
    p.add(p70("UpVectorProperty", "object", "", "", vec![]));
    p.add(p70_bool("Show", true));
    p.add(p70_bool("NegativePercentShapeSupport", true));
    p.add(p70_int("DefaultAttributeIndex", -1));
    p.add(p70_bool("Freeze", false));
    p.add(p70_bool("LODBox", false));
    for n in lcl_props([0.0; 3], [0.0; 3], [1.0; 3]) {
        p.add(n);
    }
    p.add(p70("Visibility", "Visibility", "", "A", vec![1.0.into()]));
    p.add(p70("Visibility Inheritance", "Visibility Inheritance", "", "", vec![1.into()]));
    Node::new("PropertyTemplate").p("FbxNode").child(p)
}

fn template_fbx_mesh() -> Node {
    Node::new("PropertyTemplate").p("FbxMesh").child(
        Node::new("Properties70")
            .child(p70_vec3("Color", "ColorRGB", "Color", "", [0.8, 0.8, 0.8]))
            .child(p70_vector("BBoxMin", [0.0; 3]))
            .child(p70_vector("BBoxMax", [0.0; 3]))
            .child(p70_bool("Primary Visibility", true))
            .child(p70_bool("Casts Shadows", true))
            .child(p70_bool("Receive Shadows", true)),
    )
}

fn template_fbx_phong() -> Node {
    Node::new("PropertyTemplate").p("FbxSurfacePhong").child(
        Node::new("Properties70")
            .child(p70_string("ShadingModel", "Phong"))
            .child(p70_bool("MultiLayer", false))
            .child(p70_color("EmissiveColor", [0.0; 3]))
            .child(p70_number("EmissiveFactor", 1.0))
            .child(p70_color("AmbientColor", [0.2, 0.2, 0.2]))
            .child(p70_number("AmbientFactor", 1.0))
            .child(p70_color("DiffuseColor", [0.8, 0.8, 0.8]))
            .child(p70_number("DiffuseFactor", 1.0))
            .child(p70_vector("Bump", [0.0; 3]))
            .child(p70_vector("NormalMap", [0.0; 3]))
            .child(p70_double("BumpFactor", 1.0))
            .child(p70_color("TransparentColor", [0.0; 3]))
            .child(p70_number("TransparencyFactor", 0.0))
            .child(p70_color("DisplacementColor", [0.0; 3]))
            .child(p70_double("DisplacementFactor", 1.0))
            .child(p70_color("VectorDisplacementColor", [0.0; 3]))
            .child(p70_double("VectorDisplacementFactor", 1.0))
            .child(p70_color("SpecularColor", [0.2, 0.2, 0.2]))
            .child(p70_number("SpecularFactor", 1.0))
            .child(p70_number("ShininessExponent", 20.0))
            .child(p70_color("ReflectionColor", [0.0; 3]))
            .child(p70_number("ReflectionFactor", 1.0)),
    )
}

fn template_fbx_file_texture() -> Node {
    Node::new("PropertyTemplate").p("FbxFileTexture").child(
        Node::new("Properties70")
            .child(p70_enum("TextureTypeUse", 0))
            .child(p70_number("Texture alpha", 1.0))
            .child(p70_enum("CurrentMappingType", 0))
            .child(p70_enum("WrapModeU", 0))
            .child(p70_enum("WrapModeV", 0))
            .child(p70_bool("UVSwap", false))
            .child(p70_bool("PremultiplyAlpha", true))
            .child(p70_vector("Translation", [0.0; 3]))
            .child(p70_vector("Rotation", [0.0; 3]))
            .child(p70_vector("Scaling", [1.0; 3]))
            .child(p70_vector("TextureRotationPivot", [0.0; 3]))
            .child(p70_vector("TextureScalingPivot", [0.0; 3]))
            .child(p70_enum("CurrentTextureBlendMode", 1))
            .child(p70_string("UVSet", "default"))
            .child(p70_bool("UseMaterial", false))
            .child(p70_bool("UseMipMap", false)),
    )
}

fn template_fbx_video() -> Node {
    Node::new("PropertyTemplate").p("FbxVideo").child(
        Node::new("Properties70")
            .child(p70_bool("ImageSequence", false))
            .child(p70_int("ImageSequenceOffset", 0))
            .child(p70_double("FrameRate", 0.0))
            .child(p70_int("LastFrame", 0))
            .child(p70_int("Width", 0))
            .child(p70_int("Height", 0))
            .child(p70("Path", "KString", "XRefUrl", "", vec!["".into()]))
            .child(p70_int("StartFrame", 0))
            .child(p70_int("StopFrame", 0))
            .child(p70_double("PlaySpeed", 0.0))
            .child(p70_ktime("Offset", 0))
            .child(p70_enum("InterlaceMode", 0))
            .child(p70_bool("FreeRunning", false))
            .child(p70_bool("Loop", false))
            .child(p70_enum("AccessMode", 0)),
    )
}

fn template_anim_stack() -> Node {
    Node::new("PropertyTemplate").p("FbxAnimStack").child(
        Node::new("Properties70")
            .child(p70_string("Description", ""))
            .child(p70_ktime("LocalStart", 0))
            .child(p70_ktime("LocalStop", 0))
            .child(p70_ktime("ReferenceStart", 0))
            .child(p70_ktime("ReferenceStop", 0)),
    )
}

fn template_anim_layer() -> Node {
    Node::new("PropertyTemplate").p("FbxAnimLayer").child(
        Node::new("Properties70")
            .child(p70_number("Weight", 100.0))
            .child(p70_bool("Mute", false))
            .child(p70_bool("Solo", false))
            .child(p70_bool("Lock", false))
            .child(p70_vec3("Color", "ColorRGB", "Color", "", [0.8, 0.8, 0.8]))
            .child(p70_enum("BlendMode", 0))
            .child(p70_enum("RotationAccumulationMode", 0))
            .child(p70_enum("ScaleAccumulationMode", 0))
            .child(p70("BlendModeBypass", "ULongLong", "", "", vec![0i64.into()])),
    )
}

fn template_anim_curve_node() -> Node {
    Node::new("PropertyTemplate")
        .p("FbxAnimCurveNode")
        .child(Node::new("Properties70").child(p70("d", "Compound", "", "", vec![])))
}

fn definitions(counts: &[(&'static str, usize)]) -> Node {
    let total: usize = counts.iter().map(|c| c.1).sum::<usize>() + 1;
    let mut defs = Node::new("Definitions").child(Node::with("Version", vec![100.into()])).child(Node::with("Count", vec![(total as i32).into()]));
    defs.add(Node::new("ObjectType").p("GlobalSettings").child(Node::with("Count", vec![1.into()])));
    for (ty, n) in counts {
        let mut ot = Node::new("ObjectType").p(*ty).child(Node::with("Count", vec![(*n as i32).into()]));
        match *ty {
            "Model" => ot.add(template_fbx_node()),
            "Geometry" => ot.add(template_fbx_mesh()),
            "Material" => ot.add(template_fbx_phong()),
            "Texture" => ot.add(template_fbx_file_texture()),
            "Video" => ot.add(template_fbx_video()),
            "AnimationStack" => ot.add(template_anim_stack()),
            "AnimationLayer" => ot.add(template_anim_layer()),
            "AnimationCurveNode" => ot.add(template_anim_curve_node()),
            _ => &mut ot,
        };
        defs.add(ot);
    }
    defs
}

// --------------------------------------------------------------------------------------------
// Binary serialisation
// --------------------------------------------------------------------------------------------

fn write_array<T, F: Fn(&T, &mut Vec<u8>)>(out: &mut Vec<u8>, code: u8, items: &[T], elem_size: usize, put: F) {
    out.push(code);
    out.extend_from_slice(&(items.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // encoding: raw
    out.extend_from_slice(&((items.len() * elem_size) as u32).to_le_bytes());
    out.reserve(items.len() * elem_size);
    for it in items {
        put(it, out);
    }
}

fn write_prop(out: &mut Vec<u8>, p: &P) {
    match p {
        P::I16(v) => {
            out.push(b'Y');
            out.extend_from_slice(&v.to_le_bytes());
        }
        P::Bool(v) => {
            out.push(b'C');
            out.push(*v as u8);
        }
        P::I32(v) => {
            out.push(b'I');
            out.extend_from_slice(&v.to_le_bytes());
        }
        P::F32(v) => {
            out.push(b'F');
            out.extend_from_slice(&v.to_le_bytes());
        }
        P::F64(v) => {
            out.push(b'D');
            out.extend_from_slice(&v.to_le_bytes());
        }
        P::I64(v) => {
            out.push(b'L');
            out.extend_from_slice(&v.to_le_bytes());
        }
        P::ArrF32(a) => write_array(out, b'f', a, 4, |v, o| o.extend_from_slice(&v.to_le_bytes())),
        P::ArrF64(a) => write_array(out, b'd', a, 8, |v, o| o.extend_from_slice(&v.to_le_bytes())),
        P::ArrI64(a) => write_array(out, b'l', a, 8, |v, o| o.extend_from_slice(&v.to_le_bytes())),
        P::ArrI32(a) => write_array(out, b'i', a, 4, |v, o| o.extend_from_slice(&v.to_le_bytes())),
        P::ArrBool(a) => write_array(out, b'b', a, 1, |v, o| o.push(*v as u8)),
        P::Str(s) => {
            out.push(b'S');
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        P::Raw(r) => {
            out.push(b'R');
            out.extend_from_slice(&(r.len() as u32).to_le_bytes());
            out.extend_from_slice(r);
        }
    }
}

/// Node record: EndOffset, NumProperties, PropertyListLen (u32 each), NameLen (u8), name,
/// properties, nested records and a 13-byte null record when the node has children (or is empty).
fn write_node(out: &mut Vec<u8>, node: &Node) {
    let start = out.len();
    out.extend_from_slice(&[0u8; 4]); // EndOffset (patched)
    out.extend_from_slice(&(node.props.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 4]); // PropertyListLen (patched)
    let name = node.name.as_bytes();
    out.push(name.len() as u8);
    out.extend_from_slice(name);
    let props_start = out.len();
    for p in &node.props {
        write_prop(out, p);
    }
    let props_len = (out.len() - props_start) as u32;
    if !node.children.is_empty() || node.props.is_empty() {
        for c in &node.children {
            write_node(out, c);
        }
        out.extend_from_slice(&[0u8; 13]);
    }
    let end = out.len() as u32;
    out[start..start + 4].copy_from_slice(&end.to_le_bytes());
    out[start + 8..start + 12].copy_from_slice(&props_len.to_le_bytes());
}

fn write_footer(out: &mut Vec<u8>) {
    // Footer id (the SDK derives it from the timestamp; readers do not validate it).
    out.extend_from_slice(&[0xfa, 0xbc, 0xab, 0x09, 0xd0, 0xc8, 0xd4, 0x66, 0xb1, 0x76, 0xfb, 0x83, 0x1c, 0xf7, 0x26, 0x7e]);
    out.extend_from_slice(&[0u8; 4]);
    let mut pad = (16 - (out.len() % 16)) % 16;
    if pad == 0 {
        pad = 16;
    }
    out.extend(std::iter::repeat(0u8).take(pad));
    out.extend_from_slice(&FBX_VERSION.to_le_bytes());
    out.extend_from_slice(&[0u8; 120]);
    out.extend_from_slice(&[0xf8, 0x5a, 0x8c, 0x6a, 0xde, 0xf5, 0xd9, 0x7e, 0xec, 0xe9, 0x0c, 0xe3, 0x75, 0x8f, 0x29, 0x0b]);
}
