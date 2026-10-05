fn main() {
    let p = std::env::args().nth(1).unwrap();
    let (doc, _b, _i) = gltf::import(&p).unwrap();
    println!("skins {} animations {:?} nodes {} meshes {}", doc.skins().count(), doc.animations().map(|a| (a.name().unwrap_or("?").to_string(), a.channels().count())).collect::<Vec<_>>(), doc.nodes().count(), doc.meshes().count());
    for s in doc.skins() { println!("skin joints {} skeleton {:?}", s.joints().count(), s.skeleton().map(|n| n.name().unwrap_or("?").to_string())); }
    let sc = polysquish::io::load_scene(std::path::Path::new(&p)).unwrap();
    println!("reimport: tris {} has_skin {} joints {:?} anims {}", sc.mesh.triangle_count(), sc.mesh.has_skin(), sc.skeleton.as_ref().map(|s| s.joints.len()), sc.animations.len());
    if sc.mesh.has_skin() { let sum: f32 = sc.mesh.weights[0].iter().sum(); println!("v0 joints {:?} weights {:?} sum {sum}", sc.mesh.joints[0], sc.mesh.weights[0]); }
}
