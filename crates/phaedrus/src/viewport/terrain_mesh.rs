//! Smooth terrain meshing.
//!
//! Each chunk is meshed with Surface Nets over the voxel occupancy field:
//! density samples sit at voxel centres, every cell (the cube between eight
//! samples) that the 0.5 iso-surface crosses gets one vertex at the mean of
//! its edge crossings, and every sample edge the surface crosses becomes a
//! quad joining the four cells around it. That gives the rounded look of
//! Roblox's smooth terrain from the same data.
//!
//! Level of detail: at LOD `l` a sample covers a `2^l`-voxel cube (average
//! occupancy, most common solid material), so a chunk's mesh has `8^l` fewer
//! cells. Blocks are aligned to their size, so a block never straddles a
//! chunk. Coarse chunks also mesh one extra ring of cells past their edges;
//! where they meet a finer neighbour the overlap hides the crack between the
//! two resolutions.
//!
//! Water is meshed separately against air only, so the water surface does
//! not also wrap the riverbed.

use phaedrus_dom::terrain::{CHUNK, Chunk, TerrainGrid, VOXEL_STUDS, Voxel, WATER};

/// Coarsest LOD; samples then cover 8³ voxels (32 studs).
pub const MAX_LOD: u8 = 3;
const ISO: f32 = 0.5;

#[derive(Debug, Default, Clone)]
pub struct MeshData {
    /// Chunk-local positions in studs.
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Linear RGBA; empty for water.
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct ChunkMeshes {
    pub solid: MeshData,
    pub water: MeshData,
}

#[derive(Clone, Copy, Default)]
struct Sample {
    solid: f32,
    water: f32,
    material: u8,
}

/// Meshes the chunk at `key` at `lod`. `palette` is the linear color of each
/// material slot.
pub fn mesh_chunk(grid: &TerrainGrid, palette: &[[f32; 3]; 23], key: [i32; 3], lod: u8) -> ChunkMeshes {
    let lod = lod.min(MAX_LOD);
    if !has_surface(grid, key) {
        return ChunkMeshes::default();
    }
    let step = 1i32 << lod;
    let n = CHUNK / step;
    let pad = i32::from(lod > 0);
    // Samples [lo, hi] per axis; cells [lo, hi); owned edges [-pad, n + pad).
    let lo = -pad - 1;
    let hi = n + pad;
    let size = (hi - lo + 1) as usize;
    let mut chunks = Neighborhood::new(grid, key);
    let mut samples = vec![Sample::default(); size * size * size];
    let sidx = |x: i32, y: i32, z: i32| ((x - lo) as usize) + ((y - lo) as usize) * size + ((z - lo) as usize) * size * size;
    let origin = key.map(|k| k * CHUNK);
    for z in lo..=hi {
        for y in lo..=hi {
            for x in lo..=hi {
                let v = [origin[0] + x * step, origin[1] + y * step, origin[2] + z * step];
                samples[sidx(x, y, z)] = sample_block(&mut chunks, v, step);
            }
        }
    }

    let solid_field = |s: &Sample| s.solid;
    // Solid voxels count as inside the water so the water surface meets
    // the ground instead of wrapping it.
    let water_field = |s: &Sample| if s.solid >= ISO { 1.0 } else { s.water };
    let geometry = Geometry { samples: &samples, lo, hi, pad, n, size, step };
    ChunkMeshes {
        solid: geometry.build(solid_field, |_| true, Some(palette)),
        water: if samples.iter().any(|s| s.water > 0.0) {
            geometry.build(water_field, |inside: &Sample| inside.solid < ISO, None)
        } else {
            MeshData::default()
        },
    }
}

/// False when the chunk and its neighbours are all one value, which leaves
/// nothing for the surface to cross (deep rock, open sky).
fn has_surface(grid: &TerrainGrid, key: [i32; 3]) -> bool {
    let Some(center) = grid.uniform(key) else { return true };
    for dz in -1..=1 {
        for dy in -1..=1 {
            for dx in -1..=1 {
                if grid.uniform([key[0] + dx, key[1] + dy, key[2] + dz]) != Some(center) {
                    return true;
                }
            }
        }
    }
    false
}

/// The chunk being meshed and its 26 neighbours, decoded on first use.
struct Neighborhood<'a> {
    grid: &'a TerrainGrid,
    center: [i32; 3],
    slots: [Option<Option<Chunk>>; 27],
}

impl<'a> Neighborhood<'a> {
    fn new(grid: &'a TerrainGrid, center: [i32; 3]) -> Self {
        Self { grid, center, slots: std::array::from_fn(|_| None) }
    }

    /// The chunk at `key` (within one chunk of the centre), `None` for air.
    fn get(&mut self, key: [i32; 3]) -> Option<&Chunk> {
        let rel = [0, 1, 2].map(|a| key[a] - self.center[a] + 1);
        debug_assert!(rel.iter().all(|r| (0..3).contains(r)));
        let slot = (rel[0] + rel[1] * 3 + rel[2] * 9) as usize;
        let grid = self.grid;
        self.slots[slot].get_or_insert_with(|| grid.load(key)).as_ref()
    }
}

/// Averages the `step`³ voxels starting at voxel `v` (aligned to `step`).
fn sample_block(chunks: &mut Neighborhood, v: [i32; 3], step: i32) -> Sample {
    let key = v.map(|c| c.div_euclid(CHUNK));
    let Some(chunk) = chunks.get(key) else { return Sample::default() };
    let local = v.map(|c| c.rem_euclid(CHUNK));
    let single = |vox: Voxel| {
        let d = vox.occupancy as f32 / 255.0;
        if vox.is_solid() {
            Sample { solid: d, water: 0.0, material: vox.material }
        } else if vox.material == WATER {
            Sample { solid: 0.0, water: d, material: 0 }
        } else {
            Sample::default()
        }
    };
    match chunk {
        Chunk::Uniform(vox) => single(*vox),
        Chunk::Dense(_) if step == 1 => single(chunk.get(local[0], local[1], local[2])),
        Chunk::Dense(voxels) => {
            let mut solid = 0u32;
            let mut water = 0u32;
            let mut weights = [0u32; 23];
            for y in local[1]..local[1] + step {
                for z in local[2]..local[2] + step {
                    // X is the fastest axis, so a block row is contiguous.
                    let row = Chunk::index(local[0], y, z);
                    for &vox in &voxels[row..row + step as usize] {
                        let occ = u32::from(vox.occupancy);
                        if vox.is_solid() {
                            solid += occ;
                            weights[vox.material as usize] += occ;
                        } else if vox.material == WATER {
                            water += occ;
                        }
                    }
                }
            }
            let total = (255 * step * step * step) as f32;
            let material = (0..23).max_by_key(|&m| weights[m]).unwrap_or(0) as u8;
            Sample { solid: solid as f32 / total, water: water as f32 / total, material }
        }
    }
}

struct Geometry<'a> {
    samples: &'a [Sample],
    lo: i32,
    hi: i32,
    pad: i32,
    n: i32,
    size: usize,
    step: i32,
}

/// Cube corner offsets, bit 0 = x, bit 1 = y, bit 2 = z.
const CORNERS: [[i32; 3]; 8] = [
    [0, 0, 0],
    [1, 0, 0],
    [0, 1, 0],
    [1, 1, 0],
    [0, 0, 1],
    [1, 0, 1],
    [0, 1, 1],
    [1, 1, 1],
];
const EDGES: [(usize, usize); 12] = [
    (0, 1), (2, 3), (4, 5), (6, 7),
    (0, 2), (1, 3), (4, 6), (5, 7),
    (0, 4), (1, 5), (2, 6), (3, 7),
];

impl Geometry<'_> {
    fn index(&self, x: i32, y: i32, z: i32) -> usize {
        let s = self.size;
        ((x - self.lo) as usize) + ((y - self.lo) as usize) * s + ((z - self.lo) as usize) * s * s
    }

    /// `field` maps a sample to its density; a quad is emitted only where
    /// `keep(inside sample)` holds. `palette` adds vertex colors.
    fn build(
        &self,
        field: impl Fn(&Sample) -> f32,
        keep: impl Fn(&Sample) -> bool,
        palette: Option<&[[f32; 3]; 23]>,
    ) -> MeshData {
        let mut mesh = MeshData::default();
        let cells = (self.hi - self.lo) as usize;
        let cidx = |x: i32, y: i32, z: i32| {
            ((x - self.lo) as usize) + ((y - self.lo) as usize) * cells + ((z - self.lo) as usize) * cells * cells
        };
        let density: Vec<f32> = self.samples.iter().map(&field).collect();
        let solid: Vec<bool> = density.iter().map(|&d| d > ISO).collect();
        if solid.iter().all(|&i| i) || solid.iter().all(|&i| !i) {
            return mesh;
        }
        let mut vertex_of = vec![u32::MAX; cells * cells * cells];
        let scale = self.step as f32 * VOXEL_STUDS;
        let (sy, sz) = (self.size, self.size * self.size);
        let corner_offset = CORNERS.map(|[dx, dy, dz]| dx as usize + dy as usize * sy + dz as usize * sz);

        for z in self.lo..self.hi {
            for y in self.lo..self.hi {
                for x in self.lo..self.hi {
                    let base = self.index(x, y, z);
                    let inside = corner_offset.map(|o| solid[base + o]);
                    if inside.iter().all(|&i| i) || inside.iter().all(|&i| !i) {
                        continue;
                    }
                    let corners: [&Sample; 8] = corner_offset.map(|o| &self.samples[base + o]);
                    let d = corner_offset.map(|o| density[base + o]);
                    let mut sum = [0.0f32; 3];
                    let mut count = 0.0;
                    for (a, b) in EDGES {
                        if inside[a] == inside[b] {
                            continue;
                        }
                        let t = ((ISO - d[a]) / (d[b] - d[a])).clamp(0.0, 1.0);
                        for axis in 0..3 {
                            let pa = CORNERS[a][axis] as f32;
                            let pb = CORNERS[b][axis] as f32;
                            sum[axis] += pa + (pb - pa) * t;
                        }
                        count += 1.0;
                    }
                    let p = [x as f32, y as f32, z as f32];
                    // Sample i sits at the centre of block i.
                    let pos: [f32; 3] = std::array::from_fn(|a| (p[a] + sum[a] / count + 0.5) * scale);
                    // Density gradient from the cell's edges; the normal
                    // points down it (out of the ground).
                    let grad = [
                        (d[1] - d[0]) + (d[3] - d[2]) + (d[5] - d[4]) + (d[7] - d[6]),
                        (d[2] - d[0]) + (d[3] - d[1]) + (d[6] - d[4]) + (d[7] - d[5]),
                        (d[4] - d[0]) + (d[5] - d[1]) + (d[6] - d[2]) + (d[7] - d[3]),
                    ];
                    let len = (grad[0] * grad[0] + grad[1] * grad[1] + grad[2] * grad[2]).sqrt();
                    let normal = if len > 1e-6 { grad.map(|g| -g / len) } else { [0.0, 1.0, 0.0] };
                    vertex_of[cidx(x, y, z)] = mesh.positions.len() as u32;
                    mesh.positions.push(pos);
                    mesh.normals.push(normal);
                    if let Some(palette) = palette {
                        mesh.colors.push(blend_color(&corners, palette));
                    }
                }
            }
        }

        let owned = -self.pad..self.n + self.pad;
        for z in owned.clone() {
            for y in owned.clone() {
                for x in owned.clone() {
                    let a = [x, y, z];
                    let ia = self.index(x, y, z);
                    let inside_a = solid[ia];
                    for (d, stride) in [1, sy, sz].into_iter().enumerate() {
                        let (u, v) = ((d + 1) % 3, (d + 2) % 3);
                        let ib = ia + stride;
                        if inside_a == solid[ib] || !keep(&self.samples[if inside_a { ia } else { ib }]) {
                            continue;
                        }
                        let cell = |du: i32, dv: i32| {
                            let mut c = a;
                            c[u] -= du;
                            c[v] -= dv;
                            vertex_of[cidx(c[0], c[1], c[2])]
                        };
                        let q = [cell(1, 1), cell(0, 1), cell(0, 0), cell(1, 0)];
                        if q.contains(&u32::MAX) {
                            continue;
                        }
                        // q winds counter-clockwise seen from +d; the
                        // surface faces away from the inside sample.
                        if inside_a {
                            mesh.indices.extend([q[0], q[1], q[2], q[0], q[2], q[3]]);
                        } else {
                            mesh.indices.extend([q[0], q[2], q[1], q[0], q[3], q[2]]);
                        }
                    }
                }
            }
        }
        mesh
    }
}

/// Density-weighted average of the solid corners' material colors, so
/// material borders blend over a cell like Roblox's do.
fn blend_color(corners: &[&Sample; 8], palette: &[[f32; 3]; 23]) -> [f32; 4] {
    let mut rgb = [0.0f32; 3];
    let mut weight = 0.0;
    for c in corners {
        if c.solid > 0.0 {
            let color = palette[c.material as usize];
            for i in 0..3 {
                rgb[i] += color[i] * c.solid;
            }
            weight += c.solid;
        }
    }
    if weight <= 0.0 {
        return [0.5, 0.5, 0.5, 1.0];
    }
    [rgb[0] / weight, rgb[1] / weight, rgb[2] / weight, 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use phaedrus_dom::terrain::CHUNK_VOXELS;

    const ROCK: u8 = 8;

    /// A grid whose chunk (0,0,0) is solid below voxel height `h`.
    fn flat_grid(h: i32, material: u8) -> TerrainGrid {
        let mut voxels = vec![Voxel::AIR; CHUNK_VOXELS];
        for y in 0..h {
            for z in 0..CHUNK {
                for x in 0..CHUNK {
                    voxels[Chunk::index(x, y, z)] = Voxel { material, occupancy: 255 };
                }
            }
        }
        let mut grid = TerrainGrid::default();
        grid.insert_chunk([0, 0, 0], Chunk::Dense(voxels.into_boxed_slice()));
        grid
    }

    fn palette() -> [[f32; 3]; 23] {
        let mut p = [[0.0; 3]; 23];
        p[ROCK as usize] = [0.25, 0.5, 0.75];
        p
    }

    #[test]
    fn flat_ground_is_a_level_upward_surface() {
        let grid = flat_grid(16, ROCK);
        for lod in 0..=MAX_LOD {
            let m = mesh_chunk(&grid, &palette(), [0, 0, 0], lod).solid;
            assert!(!m.is_empty(), "lod {lod}");
            // The top is at 16 voxels = 64 studs.
            let top: Vec<_> = m.normals.iter().zip(&m.positions).filter(|(n, _)| n[1] > 0.99).collect();
            assert!(!top.is_empty());
            for (_, p) in top {
                assert!((p[1] - 64.0).abs() < 0.01, "lod {lod}: top vertex at {}", p[1]);
            }
            assert!(m.colors.iter().all(|c| *c == [0.25, 0.5, 0.75, 1.0]));
        }
    }

    #[test]
    fn upward_faces_wind_counter_clockwise() {
        let grid = flat_grid(16, ROCK);
        let m = mesh_chunk(&grid, &palette(), [0, 0, 0], 0).solid;
        let mut up = 0;
        for tri in m.indices.chunks(3) {
            let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| m.positions[i as usize]);
            let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let ny = e1[2] * e2[0] - e1[0] * e2[2];
            let n = m.normals[tri[0] as usize];
            // The geometric normal agrees with the shading normal.
            let gx = e1[1] * e2[2] - e1[2] * e2[1];
            let gz = e1[0] * e2[1] - e1[1] * e2[0];
            assert!(gx * n[0] + ny * n[1] + gz * n[2] > 0.0);
            if ny > 0.0 {
                up += 1;
            }
        }
        assert!(up > 0);
    }

    #[test]
    fn coarser_lods_have_fewer_triangles() {
        let grid = flat_grid(16, ROCK);
        let counts: Vec<_> = (0..=MAX_LOD).map(|l| mesh_chunk(&grid, &palette(), [0, 0, 0], l).solid.indices.len()).collect();
        for w in counts.windows(2) {
            assert!(w[1] < w[0], "{counts:?}");
        }
    }

    #[test]
    fn buried_and_empty_chunks_produce_nothing() {
        let mut grid = TerrainGrid::default();
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    grid.insert_chunk([dx, dy, dz], Chunk::Uniform(Voxel { material: ROCK, occupancy: 255 }));
                }
            }
        }
        assert!(mesh_chunk(&grid, &palette(), [0, 0, 0], 0).solid.is_empty());
        assert!(mesh_chunk(&grid, &palette(), [10, 10, 10], 0).solid.is_empty());
    }

    #[test]
    fn water_surface_only_faces_air() {
        // Rock to 8 voxels, water to 16.
        let mut voxels = vec![Voxel::AIR; CHUNK_VOXELS];
        for y in 0..16 {
            for z in 0..CHUNK {
                for x in 0..CHUNK {
                    let material = if y < 8 { ROCK } else { WATER };
                    voxels[Chunk::index(x, y, z)] = Voxel { material, occupancy: 255 };
                }
            }
        }
        let mut grid = TerrainGrid::default();
        grid.insert_chunk([0, 0, 0], Chunk::Dense(voxels.into_boxed_slice()));
        let m = mesh_chunk(&grid, &palette(), [0, 0, 0], 0);
        assert!(!m.water.is_empty());
        // Away from the chunk's open sides, no water lies on the riverbed
        // (y = 32 studs): only the surface at 64.
        for &i in &m.water.indices {
            let p = m.water.positions[i as usize];
            if (8.0..120.0).contains(&p[0]) && (8.0..120.0).contains(&p[2]) {
                assert!((p[1] - 64.0).abs() < 0.01, "{p:?}");
            }
        }
    }
}

/// Meshing cost on real places, which are not checked in:
/// `PROBE="a.rbxl;b.rbxl" cargo test --release -p phaedrus bench -- --ignored --nocapture`
#[cfg(test)]
mod bench {
    #[test]
    #[ignore]
    fn real_places() {
        use phaedrus_dom::terrain::{TerrainAppearance, TerrainGrid, smooth_grid_bytes};
        for p in std::env::var("PROBE").unwrap().split(';') {
            let doc = phaedrus_dom::Document::open(std::path::Path::new(p)).unwrap();
            let t = doc.find_first_child_of_class(doc.workspace().unwrap(), "Terrain").unwrap();
            let palette = TerrainAppearance::from_document(&doc, t).colors;
            let grid = TerrainGrid::decode(smooth_grid_bytes(&doc, t).unwrap()).unwrap();
            let keys: Vec<_> = grid.chunk_keys().collect();
            for lod in 0..=super::MAX_LOD {
                let start = std::time::Instant::now();
                let (mut tris, mut water) = (0usize, 0usize);
                for &k in &keys {
                    let m = super::mesh_chunk(&grid, &palette, k, lod);
                    tris += m.solid.indices.len() / 3;
                    water += m.water.indices.len() / 3;
                }
                let el = start.elapsed();
                println!("{p} lod {lod}: {} chunks {:?} ({:?}/chunk) tris {tris} water {water}", keys.len(), el, el / keys.len() as u32);
            }
        }
    }
}
