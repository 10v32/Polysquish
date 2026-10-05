use glam::Vec3;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let src = polysquish::io::load_scene(std::path::Path::new(&args[1])).unwrap();
    let low = polysquish::io::load_scene(std::path::Path::new(&args[2])).unwrap();
    let bvh = polysquish::bvh::Bvh::build(&src.mesh);
    let diag = src.mesh.bounds().diagonal;
    let mut worst: Vec<(f32, usize, Vec3)> = Vec::new();
    for (i, p) in low.mesh.positions.iter().enumerate() {
        let d = bvh.closest_point(*p, diag).map(|(h, _)| h.t).unwrap_or(diag);
        worst.push((d / diag, i, *p));
    }
    worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    println!("src bounds {:?}", src.mesh.bounds());
    println!("low bounds {:?}", low.mesh.bounds());
    for w in worst.iter().take(5) { println!("{:.4} v{} {:?}", w.0, w.1, w.2); }
    // centroids
    let mut worst_c: Vec<(f32, usize)> = Vec::new();
    for t in 0..low.mesh.triangle_count() {
        let [a,b,c] = low.mesh.tri(t);
        let p = (low.mesh.positions[a as usize]+low.mesh.positions[b as usize]+low.mesh.positions[c as usize])/3.0;
        let d = bvh.closest_point(p, diag).map(|(h, _)| h.t).unwrap_or(diag);
        worst_c.push((d/diag, t));
    }
    worst_c.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    for w in worst_c.iter().take(5) { let [a,b,c]=low.mesh.tri(w.1); println!("centroid {:.4} t{} verts {:?} {:?} {:?}", w.0, w.1, low.mesh.positions[a as usize], low.mesh.positions[b as usize], low.mesh.positions[c as usize]); }
}
