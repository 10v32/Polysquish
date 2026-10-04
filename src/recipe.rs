//! The squish recipe: every knob of the pipeline, serialisable, with built-in presets.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CleanupOptions {
    pub weld: bool,
    /// Fraction of the bounding diagonal.
    pub weld_tolerance: f32,
    pub remove_degenerate: bool,
    pub remove_floaters: bool,
    /// Components with fewer than this fraction of all triangles are dropped.
    pub floater_min_fraction: f32,
    pub fix_winding: bool,
}

impl Default for CleanupOptions {
    fn default() -> Self {
        Self {
            weld: true,
            weld_tolerance: 1e-5,
            remove_degenerate: true,
            remove_floaters: true,
            floater_min_fraction: 0.001,
            fix_winding: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DecimateOptions {
    pub target_triangles: Option<usize>,
    pub target_ratio: Option<f32>,
    /// Fraction of the bounding diagonal; None = unlimited (hit the count).
    pub max_error: Option<f32>,
    pub lock_border: bool,
    pub preserve_uvs: bool,
    pub preserve_colors: bool,
    /// Use the fast "sloppy" simplifier (vertex clustering) instead of edge collapse.
    pub aggressive: bool,
}

impl Default for DecimateOptions {
    fn default() -> Self {
        Self {
            target_triangles: Some(30_000),
            target_ratio: None,
            max_error: None,
            lock_border: true,
            preserve_uvs: true,
            preserve_colors: true,
            aggressive: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UvOptions {
    pub enabled: bool,
    pub resolution: u32,
    pub padding: u32,
    pub keep_existing_if_good: bool,
}

impl Default for UvOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            resolution: 2048,
            padding: 4,
            keep_existing_if_good: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NormalConvention {
    OpenGL,
    DirectX,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BakeOptions {
    pub enabled: bool,
    pub resolution: u32,
    pub normal_map: bool,
    pub albedo: bool,
    pub ao: bool,
    pub ao_samples: u32,
    pub metallic_roughness: bool,
    pub normal_convention: NormalConvention,
    /// Max ray distance as a fraction of the bounding diagonal; None = auto.
    pub ray_distance: Option<f32>,
    pub dilation_px: u32,
    pub supersample: u32,
}

impl Default for BakeOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            resolution: 2048,
            normal_map: true,
            albedo: true,
            ao: true,
            ao_samples: 32,
            metallic_roughness: true,
            normal_convention: NormalConvention::OpenGL,
            ray_distance: None,
            dilation_px: 8,
            supersample: 2,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LodOptions {
    pub count: usize,
    pub ratios: Vec<f32>,
}

impl Default for LodOptions {
    fn default() -> Self {
        Self {
            count: 3,
            ratios: vec![0.5, 0.25, 0.1],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CollisionOptions {
    pub convex_hull: bool,
    #[serde(rename = "box")]
    pub bbox: bool,
    pub simplified_mesh: bool,
    pub simplified_triangles: usize,
}

impl Default for CollisionOptions {
    fn default() -> Self {
        Self {
            convex_hull: true,
            bbox: true,
            simplified_mesh: true,
            simplified_triangles: 300,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Generic,
    Unity,
    Unreal,
    Godot,
    Blender,
    Maya,
    C4d,
}

impl Target {
    pub fn label(self) -> &'static str {
        match self {
            Target::Generic => "Generic",
            Target::Unity => "Unity",
            Target::Unreal => "Unreal Engine",
            Target::Godot => "Godot",
            Target::Blender => "Blender",
            Target::Maya => "Maya",
            Target::C4d => "Cinema 4D",
        }
    }
    /// Scale applied on export when the recipe scale is left at 1.0 and the source looks like metres.
    pub fn prefers_centimeters(self) -> bool {
        matches!(self, Target::Unreal | Target::Maya | Target::C4d)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ExportOptions {
    pub glb: bool,
    pub obj: bool,
    pub report: bool,
    pub scale: f32,
    pub target: Target,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            glb: true,
            obj: true,
            report: true,
            scale: 1.0,
            target: Target::Generic,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Recipe {
    pub preset: String,
    pub cleanup: CleanupOptions,
    pub decimate: DecimateOptions,
    pub uv: UvOptions,
    pub bake: BakeOptions,
    pub lods: LodOptions,
    pub collision: CollisionOptions,
    pub export: ExportOptions,
    pub seed: u64,
}

impl Default for Recipe {
    fn default() -> Self {
        Self {
            preset: "hero".into(),
            cleanup: Default::default(),
            decimate: Default::default(),
            uv: Default::default(),
            bake: Default::default(),
            lods: Default::default(),
            collision: Default::default(),
            export: Default::default(),
            seed: 1337,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub tagline: String,
    pub description: String,
    pub icon: String,
    pub target_triangles: usize,
    pub texture_size: u32,
    pub recipe: Recipe,
}

impl Recipe {
    pub fn with_budget(mut self, preset: &str, triangles: usize, texture: u32) -> Self {
        self.preset = preset.into();
        self.decimate.target_triangles = Some(triangles);
        self.uv.resolution = texture;
        self.bake.resolution = texture;
        self
    }

    pub fn preset(id: &str) -> Option<Recipe> {
        presets().into_iter().find(|p| p.id == id).map(|p| p.recipe)
    }
}

pub fn presets() -> Vec<Preset> {
    let hero = Recipe::default().with_budget("hero", 30_000, 2048);
    let prop = Recipe::default().with_budget("prop", 5_000, 1024);
    let mut mobile = Recipe::default().with_budget("mobile", 1_500, 1024);
    mobile.bake.ao = true;
    mobile.bake.metallic_roughness = false;
    mobile.lods = LodOptions { count: 2, ratios: vec![0.5, 0.25] };
    let mut dcc = Recipe::default().with_budget("dcc", 150_000, 4096);
    dcc.lods = LodOptions { count: 0, ratios: vec![] };
    dcc.collision = CollisionOptions {
        convex_hull: false,
        bbox: false,
        simplified_mesh: false,
        simplified_triangles: 300,
    };
    dcc.bake.ao = false;
    dcc.export.target = Target::Blender;

    vec![
        Preset {
            id: "hero".into(),
            name: "Hero asset".into(),
            tagline: "PC / console close-up".into(),
            description: "30k triangles, 2K normal + albedo + AO, 3 LODs, collision. For characters and props the camera gets close to.".into(),
            icon: "sparkles".into(),
            target_triangles: 30_000,
            texture_size: 2048,
            recipe: hero,
        },
        Preset {
            id: "prop".into(),
            name: "Environment prop".into(),
            tagline: "Clutter and set dressing".into(),
            description: "5k triangles, 1K textures, 3 LODs, collision. Fills a scene without eating the frame budget.".into(),
            icon: "cube".into(),
            target_triangles: 5_000,
            texture_size: 1024,
            recipe: prop,
        },
        Preset {
            id: "mobile".into(),
            name: "Mobile / VR".into(),
            tagline: "Every triangle counts".into(),
            description: "1.5k triangles, 1K albedo + normal, 2 LODs. Tuned for phones and standalone headsets.".into(),
            icon: "phone".into(),
            target_triangles: 1_500,
            texture_size: 1024,
            recipe: mobile,
        },
        Preset {
            id: "dcc".into(),
            name: "Clean for Blender / Maya / C4D".into(),
            tagline: "Lighter, not lossy".into(),
            description: "150k triangles, 4K textures, no LODs. Keeps detail for further sculpting, rigging or rendering.".into(),
            icon: "brush".into(),
            target_triangles: 150_000,
            texture_size: 4096,
            recipe: dcc,
        },
    ]
}
