//! Smooth terrain voxel data (`Terrain.SmoothGrid`).
//!
//! `SmoothGrid` is a binary string with this layout:
//!
//! - Byte 0 is the format version (1), byte 1 is log2 of the chunk edge (5,
//!   so chunks are 32³ voxels). An empty grid is just those two bytes.
//! - Chunks follow until the end. Each starts with its offset from the
//!   previous chunk (the first from 0,0,0), in chunk units, as three
//!   big-endian 32-bit two's-complement integers whose bytes are interleaved
//!   by significance: the top byte of X, Y, Z, then the next byte of each,
//!   and so on.
//! - Then run records until the chunk's 32³ voxels are filled, X fastest,
//!   then Z, then Y. A record's lead byte holds the material slot in its low
//!   six bits. Bit 6 means an occupancy byte follows; without it, Air is
//!   empty and anything else is full. Bit 7 means a count byte follows and
//!   the run is count + 1 voxels. A count of 0 is a single voxel followed by
//!   one more byte (its liquid share), which is skipped here.
//!
//! Voxels are 4 studs on a side and the grid is aligned to the world origin.

use std::collections::HashMap;

use anyhow::{Result, bail, ensure};
use rbx_types::{Color3, MaterialColors, TerrainMaterials, Variant};

use crate::geometry::color3uint8_to_f32;

/// Voxels along a chunk edge.
pub const CHUNK: i32 = 32;
const CHUNK_LOG2: u8 = 5;
/// Voxels in one chunk.
pub const CHUNK_VOXELS: usize = (CHUNK * CHUNK * CHUNK) as usize;
/// Studs along a voxel edge.
pub const VOXEL_STUDS: f32 = 4.0;

const VERSION: u8 = 1;
const HAS_OCCUPANCY: u8 = 0x40;
const HAS_COUNT: u8 = 0x80;
const MATERIAL_BITS: u8 = 0x3f;
/// Terrain spans at most ±32k studs; chunk keys past this are corrupt data.
const MAX_CHUNK: i32 = 32_000 / (CHUNK * VOXEL_STUDS as i32) + 1;

/// Terrain material names by `SmoothGrid` slot. This is the same order
/// `MaterialColors` stores its colors in (after Air and Water).
pub const MATERIALS: [&str; 23] = [
    "Air",
    "Water",
    "Grass",
    "Slate",
    "Concrete",
    "Brick",
    "Sand",
    "WoodPlanks",
    "Rock",
    "Glacier",
    "Snow",
    "Sandstone",
    "Mud",
    "Basalt",
    "Ground",
    "CrackedLava",
    "Asphalt",
    "Cobblestone",
    "Ice",
    "LeafyGrass",
    "Salt",
    "Limestone",
    "Pavement",
];

pub const AIR: u8 = 0;
pub const WATER: u8 = 1;

/// One terrain voxel: a material slot and how full the voxel is (0..=255).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Voxel {
    pub material: u8,
    pub occupancy: u8,
}

impl Voxel {
    pub const AIR: Voxel = Voxel { material: AIR, occupancy: 0 };

    pub fn is_solid(self) -> bool {
        self.material > WATER && self.occupancy > 0
    }

    pub fn is_water(self) -> bool {
        self.material == WATER && self.occupancy > 0
    }
}

/// A chunk's voxels. Chunks deep underground or in open air are a single
/// value, which saves the 64 KB a dense chunk costs.
#[derive(Debug, Clone, PartialEq)]
pub enum Chunk {
    Uniform(Voxel),
    Dense(Box<[Voxel]>),
}

impl Chunk {
    /// Index of a chunk-local voxel in storage order.
    #[inline]
    pub fn index(x: i32, y: i32, z: i32) -> usize {
        (x + z * CHUNK + y * CHUNK * CHUNK) as usize
    }

    #[inline]
    pub fn get(&self, x: i32, y: i32, z: i32) -> Voxel {
        match self {
            Chunk::Uniform(v) => *v,
            Chunk::Dense(v) => v[Self::index(x, y, z)],
        }
    }

    pub fn uniform(&self) -> Option<Voxel> {
        match self {
            Chunk::Uniform(v) => Some(*v),
            Chunk::Dense(_) => None,
        }
    }
}

/// Where a chunk's voxels live: a single value, or a byte range of run
/// records in the `SmoothGrid` blob, decoded on demand.
#[derive(Debug, Clone, PartialEq)]
enum Stored {
    Uniform(Voxel),
    Encoded(std::ops::Range<usize>),
    Decoded(Chunk),
}

/// An indexed `SmoothGrid`: a sparse map of chunks keyed by chunk
/// coordinate. Chunks missing from the map are air.
///
/// Surface chunks are kept run-length encoded (a dense chunk is 64 KB and
/// large maps have tens of thousands of them); [`TerrainGrid::load`] expands
/// one when it is needed.
#[derive(Debug, Clone, Default)]
pub struct TerrainGrid {
    bytes: std::sync::Arc<[u8]>,
    chunks: HashMap<[i32; 3], Stored>,
}

impl TerrainGrid {
    /// Indexes a `SmoothGrid` binary string, validating every chunk.
    pub fn decode(bytes: impl Into<std::sync::Arc<[u8]>>) -> Result<Self> {
        let bytes: std::sync::Arc<[u8]> = bytes.into();
        ensure!(bytes.len() >= 2, "SmoothGrid has no header");
        ensure!(bytes[0] == VERSION, "unsupported SmoothGrid version {}", bytes[0]);
        ensure!(bytes[1] == CHUNK_LOG2, "unsupported SmoothGrid chunk size 2^{}", bytes[1]);
        let mut reader = Reader { bytes: &bytes, at: 2 };
        let mut chunks = HashMap::new();
        let mut key = [0i32; 3];
        while reader.at < bytes.len() {
            let mut delta = [0u32; 3];
            for _ in 0..4 {
                for axis in &mut delta {
                    *axis = (*axis << 8) | u32::from(reader.byte()?);
                }
            }
            for (k, d) in key.iter_mut().zip(delta) {
                *k = k.wrapping_add(d as i32);
            }
            ensure!(key.iter().all(|k| k.abs() <= MAX_CHUNK), "SmoothGrid chunk {key:?} is out of range");
            let start = reader.at;
            // Most chunks are one long run (solid rock or open air).
            let mut first = None;
            let mut uniform = true;
            walk_chunk(&mut reader, |v, _, _| {
                uniform &= *first.get_or_insert(v) == v;
            })?;
            let stored = match first {
                Some(v) if uniform => Stored::Uniform(v),
                _ => Stored::Encoded(start..reader.at),
            };
            // All-air chunks need no storage.
            if stored != Stored::Uniform(Voxel::AIR) && chunks.insert(key, stored).is_some() {
                bail!("SmoothGrid repeats chunk {key:?}");
            }
        }
        Ok(Self { bytes, chunks })
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    pub fn contains_chunk(&self, key: [i32; 3]) -> bool {
        self.chunks.contains_key(&key)
    }

    /// The chunk's voxels, decoding them if they are stored encoded.
    pub fn load(&self, key: [i32; 3]) -> Option<Chunk> {
        Some(match self.chunks.get(&key)? {
            Stored::Uniform(v) => Chunk::Uniform(*v),
            Stored::Decoded(c) => c.clone(),
            Stored::Encoded(range) => {
                let mut reader = Reader { bytes: &self.bytes[..range.end], at: range.start };
                let mut voxels = vec![Voxel::AIR; CHUNK_VOXELS];
                walk_chunk(&mut reader, |v, at, run| voxels[at..at + run].fill(v))
                    .expect("validated when indexed");
                Chunk::Dense(voxels.into_boxed_slice())
            }
        })
    }

    /// The chunk's single value, when it has one.
    pub fn uniform(&self, key: [i32; 3]) -> Option<Voxel> {
        match self.chunks.get(&key) {
            None => Some(Voxel::AIR),
            Some(Stored::Uniform(v)) => Some(*v),
            Some(Stored::Decoded(c)) => c.uniform(),
            Some(Stored::Encoded(_)) => None,
        }
    }

    pub fn chunk_keys(&self) -> impl Iterator<Item = [i32; 3]> + '_ {
        self.chunks.keys().copied()
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Chunks that need a decode to read (not a single value).
    pub fn encoded_chunk_count(&self) -> usize {
        self.chunks.values().filter(|s| matches!(s, Stored::Encoded(_))).count()
    }

    /// The voxel at a world voxel coordinate. Decodes the chunk on every
    /// call; use [`VoxelReader`] for repeated reads.
    pub fn voxel(&self, v: [i32; 3]) -> Voxel {
        VoxelReader::new(self).voxel(v)
    }

    /// Inserts already-decoded voxels (generated terrain, tests).
    pub fn insert_chunk(&mut self, key: [i32; 3], chunk: Chunk) {
        self.chunks.insert(key, Stored::Decoded(chunk));
    }
}

/// Reads voxels from a [`TerrainGrid`], keeping the chunks it touched
/// decoded.
pub struct VoxelReader<'a> {
    grid: &'a TerrainGrid,
    cache: HashMap<[i32; 3], Option<std::sync::Arc<Chunk>>>,
}

impl<'a> VoxelReader<'a> {
    pub fn new(grid: &'a TerrainGrid) -> Self {
        Self { grid, cache: HashMap::new() }
    }

    /// The decoded chunk, or `None` for air.
    pub fn chunk(&mut self, key: [i32; 3]) -> Option<std::sync::Arc<Chunk>> {
        let grid = self.grid;
        self.cache.entry(key).or_insert_with(|| grid.load(key).map(std::sync::Arc::new)).clone()
    }

    pub fn voxel(&mut self, v: [i32; 3]) -> Voxel {
        let key = v.map(|c| c.div_euclid(CHUNK));
        match self.chunk(key) {
            Some(chunk) => {
                let [x, y, z] = v.map(|c| c.rem_euclid(CHUNK));
                chunk.get(x, y, z)
            }
            None => Voxel::AIR,
        }
    }
}

/// Walks one chunk's run records, calling `visit(voxel, first index, run)`.
fn walk_chunk(reader: &mut Reader, mut visit: impl FnMut(Voxel, usize, usize)) -> Result<()> {
    let mut filled = 0usize;
    while filled < CHUNK_VOXELS {
        let lead = reader.byte()?;
        let material = lead & MATERIAL_BITS;
        ensure!((material as usize) < MATERIALS.len(), "SmoothGrid material slot {material} is unknown");
        let occupancy = if lead & HAS_OCCUPANCY != 0 {
            reader.byte()?
        } else if material == AIR {
            0
        } else {
            255
        };
        let run = if lead & HAS_COUNT != 0 {
            match reader.byte()? {
                0 => {
                    reader.byte()?; // liquid share
                    1
                }
                n => n as usize + 1,
            }
        } else {
            1
        };
        ensure!(filled + run <= CHUNK_VOXELS, "SmoothGrid run overflows its chunk");
        let voxel = Voxel { material, occupancy: if material == AIR { 0 } else { occupancy } };
        visit(voxel, filled, run);
        filled += run;
    }
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8> {
        let Some(&b) = self.bytes.get(self.at) else { bail!("SmoothGrid is truncated") };
        self.at += 1;
        Ok(b)
    }
}

/// How terrain is colored: one Color3 per material slot (from
/// `Terrain.MaterialColors`) plus the water settings.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainAppearance {
    /// sRGB color per material slot. Air's entry is unused; Water's is
    /// `WaterColor`.
    pub colors: [[f32; 3]; 23],
    pub water_transparency: f32,
    pub water_reflectance: f32,
}

impl Default for TerrainAppearance {
    fn default() -> Self {
        Self::from_material_colors(&MaterialColors::new(), [12.0 / 255.0, 84.0 / 255.0, 92.0 / 255.0], 0.3, 1.0)
    }
}

impl TerrainAppearance {
    pub fn from_material_colors(colors: &MaterialColors, water: [f32; 3], water_transparency: f32, water_reflectance: f32) -> Self {
        let mut out = [[0.0; 3]; 23];
        out[WATER as usize] = water;
        for (slot, name) in MATERIALS.iter().enumerate().skip(2) {
            if let Ok(m) = name.parse::<TerrainMaterials>() {
                out[slot] = color3uint8_to_f32(colors.get_color(m));
            }
        }
        Self { colors: out, water_transparency, water_reflectance }
    }

    /// Reads the appearance properties of a Terrain instance.
    pub fn from_document(doc: &crate::Document, terrain: rbx_types::Ref) -> Self {
        let colors = match doc.get_property(terrain, "MaterialColors") {
            Some(Variant::MaterialColors(c)) => c,
            _ => MaterialColors::new(),
        };
        let water = match doc.get_property(terrain, "WaterColor") {
            Some(Variant::Color3(Color3 { r, g, b })) => [r, g, b],
            _ => Self::default().colors[WATER as usize],
        };
        let float = |name, default| match doc.get_property(terrain, name) {
            Some(Variant::Float32(v)) => v.clamp(0.0, 1.0),
            _ => default,
        };
        Self::from_material_colors(&colors, water, float("WaterTransparency", 0.3), float("WaterReflectance", 1.0))
    }
}

/// Properties of Terrain that change how it renders.
pub fn is_render_property(name: &str) -> bool {
    matches!(name, "SmoothGrid" | "MaterialColors" | "WaterColor" | "WaterTransparency" | "WaterReflectance")
}

/// Reads `Terrain.SmoothGrid`'s raw bytes.
pub fn smooth_grid_bytes(doc: &crate::Document, terrain: rbx_types::Ref) -> Option<Vec<u8>> {
    match doc.get_raw(terrain, "SmoothGrid")? {
        Variant::BinaryString(b) => Some(AsRef::<[u8]>::as_ref(&b).to_vec()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_key(out: &mut Vec<u8>, delta: [i32; 3]) {
        for shift in [24, 16, 8, 0] {
            out.extend(delta.iter().map(|d| ((*d as u32) >> shift) as u8));
        }
    }

    #[test]
    fn empty_grid() {
        let grid = TerrainGrid::decode(vec![1u8, 5]).unwrap();
        assert!(grid.is_empty());
        assert!(TerrainGrid::decode(vec![2u8, 5]).is_err());
        assert!(TerrainGrid::decode(vec![1u8]).is_err());
    }

    #[test]
    fn decodes_uniform_and_dense_chunks() {
        let mut bytes = vec![1u8, 5];
        // Chunk (-1, 0, 2): all Rock. 32768 = 128 runs of 256.
        encode_key(&mut bytes, [-1, 0, 2]);
        for _ in 0..128 {
            bytes.extend([8 | HAS_COUNT, 255]);
        }
        // Chunk (0, 0, 2) (delta +1 in X): one half-full Grass voxel, a
        // single Water voxel with a liquid byte, then air.
        encode_key(&mut bytes, [1, 0, 0]);
        bytes.extend([2 | HAS_OCCUPANCY, 128]);
        bytes.extend([1 | HAS_COUNT, 0, 77]);
        let mut left = CHUNK_VOXELS - 2;
        while left > 0 {
            let run = left.min(256);
            bytes.extend([HAS_COUNT, (run - 1) as u8]);
            left -= run;
        }
        let grid = TerrainGrid::decode(bytes).unwrap();
        assert_eq!(grid.chunk_count(), 2);
        assert_eq!(grid.uniform([-1, 0, 2]), Some(Voxel { material: 8, occupancy: 255 }));
        assert_eq!(grid.encoded_chunk_count(), 1);
        assert_eq!(grid.voxel([-1, 5, 64]), Voxel { material: 8, occupancy: 255 });
        assert_eq!(grid.voxel([0, 0, 64]), Voxel { material: 2, occupancy: 128 });
        assert_eq!(grid.voxel([1, 0, 64]), Voxel { material: WATER, occupancy: 255 });
        assert_eq!(grid.voxel([2, 0, 64]), Voxel::AIR);
        assert_eq!(grid.voxel([500, 0, 0]), Voxel::AIR);
    }

    #[test]
    fn rejects_overflowing_runs() {
        let mut bytes = vec![1u8, 5];
        encode_key(&mut bytes, [0, 0, 0]);
        for _ in 0..129 {
            bytes.extend([8 | HAS_COUNT, 255]);
        }
        assert!(TerrainGrid::decode(bytes).is_err());
    }

    #[test]
    fn default_palette_matches_roblox() {
        let a = TerrainAppearance::default();
        let grass = a.colors[2];
        assert!((grass[0] - 106.0 / 255.0).abs() < 1e-6);
        assert!((a.colors[22][0] - 148.0 / 255.0).abs() < 1e-6, "Pavement");
    }
}
