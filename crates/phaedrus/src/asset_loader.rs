//! Downloads a place's union meshes, MeshPart meshes and textures into the
//! asset cache after it opens.
//!
//! Needs an Open Cloud API key (Settings > API Manager). Without one the
//! step is skipped with a warning: unions and MeshParts render as bounding
//! boxes and only locally available textures show.
//! Downloaded files stay in the cache, so reopening a place only fetches
//! what is still missing.
//!
//! Everything slow happens on the download threads: the request, the cache
//! write, and decoding the mesh or image. The main thread only receives
//! decoded [`Arrival`]s (see [`AssetDownloads::take_arrivals`]) and uploads
//! them, so downloads do not stall the viewport.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bevy::prelude::*;
use parking_lot::Mutex;
use phaedrus_dom::content::{AssetKind, ImageAsset, cache_file, classify_image, missing_assets};
use phaedrus_dom::mesh::{DecodedMesh, decode_mesh};
use phaedrus_dom::union::decode_union_asset;
use phaedrus_luau::{OutputLevel, OutputLine, SharedOutput};

use crate::assets::{ContentProvider, decode_image};
use crate::opencloud::{ApiAuth, ApiError, ApiKey, OpenCloud};
use crate::studio::Studio;

const WORKERS: usize = 6;
const MAX_RETRIES: u32 = 5;

pub const NO_AUTH_WARNING: &str = "Assets cannot load without user auth: add an Open Cloud API key in \
    File > Phaedrus Settings > API Manager.";

/// Fetches one asset's bytes. Implemented by [`OpenCloud`]; tests use fakes.
pub trait Fetch: Send + Sync + 'static {
    fn fetch(&self, id: u64) -> Result<Vec<u8>, ApiError>;
}

impl Fetch for OpenCloud {
    fn fetch(&self, id: u64) -> Result<Vec<u8>, ApiError> {
        self.download_asset(id)
    }
}

#[derive(Default)]
struct Progress {
    total: usize,
    done: usize,
    skipped: usize,
    unsupported: usize,
    failed: usize,
    workers_left: usize,
}

/// An asset that finished downloading, already decoded off the main thread.
pub struct Arrival {
    pub id: u64,
    pub kind: AssetKind,
    /// `None` if it could not be decoded; the renderer then falls back to
    /// reading the cache file itself, which reports the error.
    pub decoded: Option<Decoded>,
}

pub enum Decoded {
    /// A union mesh (in studs at its `InitialSize`) or a `.mesh` in its own
    /// units; either still needs fitting to its part.
    Mesh(DecodedMesh),
    Image(Image),
}

struct Job {
    queue: Mutex<VecDeque<(u64, AssetKind)>>,
    progress: Mutex<Progress>,
    cancel: AtomicBool,
    arrivals: Arc<Mutex<Vec<Arrival>>>,
    cache_dir: PathBuf,
    output: SharedOutput,
}

#[derive(Resource, Default)]
pub struct AssetDownloads {
    job: Option<Arc<Job>>,
    arrivals: Arc<Mutex<Vec<Arrival>>>,
    opened_seen: u64,
}

impl AssetDownloads {
    /// Assets that finished downloading since the last call.
    pub fn take_arrivals(&self) -> Vec<Arrival> {
        std::mem::take(&mut *self.arrivals.lock())
    }

    /// `(finished, total)` while a download is running.
    pub fn progress(&self) -> Option<(usize, usize)> {
        let job = self.job.as_ref()?;
        let p = job.progress.lock();
        (p.workers_left > 0).then_some((p.done + p.skipped + p.unsupported + p.failed, p.total))
    }

    fn cancel(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Starts downloading `ids`, or logs why it cannot. Returns whether a
    /// download started.
    fn begin(
        &mut self,
        ids: Vec<(u64, AssetKind)>,
        key: Option<&ApiKey>,
        cache_dir: Option<&Path>,
        output: &SharedOutput,
        client: impl FnOnce(ApiKey) -> Arc<dyn Fetch>,
    ) -> bool {
        self.cancel();
        if ids.is_empty() {
            return false;
        }
        let Some(key) = key else {
            log(output, OutputLevel::Warn, format!("{NO_AUTH_WARNING} {} skipped.", describe(&ids)));
            return false;
        };
        let Some(cache_dir) = cache_dir else {
            log(output, OutputLevel::Warn, "No asset cache folder (%APPDATA% is not set); skipping asset downloads.");
            return false;
        };
        if let Err(e) = std::fs::create_dir_all(cache_dir) {
            log(output, OutputLevel::Warn, format!("Could not create {}: {e}; skipping asset downloads.", cache_dir.display()));
            return false;
        }
        log(output, OutputLevel::Info, format!("Downloading {} from Open Cloud...", describe(&ids)));
        let workers = WORKERS.min(ids.len());
        let job = Arc::new(Job {
            progress: Mutex::new(Progress { total: ids.len(), workers_left: workers, ..default() }),
            queue: Mutex::new(ids.into()),
            cancel: AtomicBool::new(false),
            arrivals: self.arrivals.clone(),
            cache_dir: cache_dir.to_owned(),
            output: output.clone(),
        });
        let client = client(key.clone());
        for i in 0..workers {
            let (thread_job, client) = (job.clone(), client.clone());
            let spawned =
                std::thread::Builder::new().name(format!("asset-download-{i}")).spawn(move || worker(&thread_job, &*client));
            if spawned.is_err() {
                // Account for the worker that never ran so the summary still prints.
                job.progress.lock().workers_left -= 1;
            }
        }
        self.job = Some(job);
        true
    }
}

/// "3 union meshes, 1 mesh, 12 images".
fn describe(ids: &[(u64, AssetKind)]) -> String {
    [AssetKind::UnionMesh, AssetKind::Mesh, AssetKind::Image]
        .into_iter()
        .filter_map(|kind| {
            let n = ids.iter().filter(|(_, k)| *k == kind).count();
            (n > 0).then(|| {
                let label = kind.label();
                if n == 1 { format!("1 {label}") } else { format!("{n} {}", plural(label)) }
            })
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn plural(label: &str) -> String {
    if label.ends_with('h') { format!("{label}es") } else { format!("{label}s") }
}

fn log(output: &SharedOutput, level: OutputLevel, text: impl Into<String>) {
    output.lock().push(OutputLine {
        level,
        text: text.into(),
        time: std::time::SystemTime::now(),
        source: Some("Assets".into()),
    });
}

fn worker(job: &Job, client: &dyn Fetch) {
    while !job.cancel.load(Ordering::Relaxed) {
        let Some((id, kind)) = job.queue.lock().pop_front() else { break };
        let result = fetch_with_retry(job, client, id).and_then(|bytes| prepare(job, client, id, kind, bytes));
        let mut p = job.progress.lock();
        match result {
            Ok(None) => p.unsupported += 1,
            Ok(Some((path, bytes))) => match write_atomic(&path, &bytes) {
                Ok(()) => {
                    p.done += 1;
                    drop(p);
                    let decoded = decode(kind, &path, bytes);
                    job.arrivals.lock().push(Arrival { id, kind, decoded });
                    continue;
                }
                Err(e) => {
                    p.failed += 1;
                    log(&job.output, OutputLevel::Warn, format!("Could not cache asset {id}: {e}"));
                }
            },
            // Assets the key's owner cannot access, or that no longer exist.
            Err(ApiError::Forbidden | ApiError::NotFound) => p.skipped += 1,
            Err(ApiError::Unauthorized) => {
                p.failed += 1;
                if !job.cancel.swap(true, Ordering::Relaxed) {
                    log(&job.output, OutputLevel::Error, format!("Asset downloads stopped. {}", ApiError::Unauthorized));
                }
            }
            Err(e) => {
                p.failed += 1;
                if p.failed <= 3 {
                    log(&job.output, OutputLevel::Warn, format!("Asset {id}: {e}"));
                }
            }
        }
    }
    let mut p = job.progress.lock();
    p.workers_left -= 1;
    if p.workers_left == 0 && (!job.cancel.load(Ordering::Relaxed) || p.done > 0) {
        log(
            &job.output,
            if p.failed > 0 { OutputLevel::Warn } else { OutputLevel::Info },
            format!(
                "Asset download finished: {} downloaded, {} not accessible with this key, {} in unsupported formats, {} failed.",
                p.done, p.skipped, p.unsupported, p.failed
            ),
        );
    }
}

/// Decodes a freshly cached asset so the main thread only has to upload it.
fn decode(kind: AssetKind, path: &Path, bytes: Vec<u8>) -> Option<Decoded> {
    match kind {
        AssetKind::UnionMesh => decode_union_asset(bytes).ok().flatten().map(Decoded::Mesh),
        AssetKind::Mesh => decode_mesh(&bytes).ok().map(Decoded::Mesh),
        AssetKind::Image => {
            let ext = path.extension()?.to_str()?;
            decode_image(&bytes, ext).map(Decoded::Image)
        }
    }
}

/// Where and what to cache for a downloaded asset; `None` if its format is
/// not supported. Decal assets are models that point at their image, which
/// is fetched and cached under the Decal's id.
fn prepare(job: &Job, client: &dyn Fetch, id: u64, kind: AssetKind, bytes: Vec<u8>) -> Result<Option<(PathBuf, Vec<u8>)>, ApiError> {
    if kind != AssetKind::Image {
        return Ok(Some((cache_file(&job.cache_dir, id, kind), bytes)));
    }
    let (ext, bytes) = match classify_image(&bytes) {
        ImageAsset::Image(ext) => (ext, bytes),
        ImageAsset::Redirect(inner) if inner != id => {
            let bytes = fetch_with_retry(job, client, inner)?;
            match classify_image(&bytes) {
                ImageAsset::Image(ext) => (ext, bytes),
                _ => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    Ok(Some((job.cache_dir.join(format!("{id}.{ext}")), bytes)))
}

fn fetch_with_retry(job: &Job, client: &dyn Fetch, id: u64) -> Result<Vec<u8>, ApiError> {
    let mut attempt = 0;
    loop {
        match client.fetch(id) {
            Err(ApiError::RateLimited { retry_after }) if attempt < MAX_RETRIES => {
                let wait = retry_after.unwrap_or(Duration::from_secs(1 << attempt)).min(Duration::from_secs(30));
                attempt += 1;
                std::thread::sleep(wait);
                if job.cancel.load(Ordering::Relaxed) {
                    return Err(ApiError::RateLimited { retry_after: None });
                }
            }
            r => return r,
        }
    }
}

/// Writes via a temp file so a cancelled download never leaves a truncated
/// file that looks cached.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Starts fetching a place's missing assets once it has opened.
pub fn start_downloads(
    studio: Res<Studio>,
    auth: Res<ApiAuth>,
    content: Res<ContentProvider>,
    mut downloads: ResMut<AssetDownloads>,
) {
    if downloads.opened_seen == studio.opened {
        return;
    }
    downloads.opened_seen = studio.opened;
    let cache_dir = content.asset_cache_dir();
    let mut ids = {
        let doc = studio.doc();
        missing_assets(doc.dom().descendants(), cache_dir)
    };
    // Geometry first: it changes the scene more than textures do.
    ids.sort_by_key(|&(id, kind)| (kind, id));
    downloads.begin(ids, auth.key(), cache_dir, &studio.output, |key| Arc::new(OpenCloud::new(key)));
}

/// Shows download progress in the status bar.
pub fn report_progress(downloads: Res<AssetDownloads>, mut studio: ResMut<Studio>, mut was_active: Local<bool>) {
    match downloads.progress() {
        Some((finished, total)) => {
            *was_active = true;
            let text = format!("Downloading assets {finished}/{total}");
            if studio.status != text {
                studio.status = text;
            }
        }
        None if *was_active => {
            *was_active = false;
            studio.status = String::new();
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(fn(u64) -> Result<Vec<u8>, ApiError>);
    impl Fetch for Fake {
        fn fetch(&self, id: u64) -> Result<Vec<u8>, ApiError> {
            (self.0)(id)
        }
    }

    fn lines(out: &SharedOutput) -> Vec<(OutputLevel, String)> {
        out.lock().iter().map(|l| (l.level, l.text.clone())).collect()
    }

    fn wait(d: &AssetDownloads) {
        let start = std::time::Instant::now();
        while d.job.as_ref().is_some_and(|j| j.progress.lock().workers_left > 0) {
            assert!(start.elapsed() < Duration::from_secs(10), "downloads did not finish");
            std::thread::yield_now();
        }
    }

    #[test]
    fn without_key_warns_once_and_never_fetches() {
        let out = SharedOutput::default();
        let mut d = AssetDownloads::default();
        let started = d.begin(vec![(1, AssetKind::UnionMesh), (2, AssetKind::Mesh), (3, AssetKind::Image)], None, Some(Path::new(".")), &out, |_| -> Arc<dyn Fetch> {
            panic!("no client without a key")
        });
        assert!(!started);
        let l = lines(&out);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].0, OutputLevel::Warn);
        assert!(l[0].1.starts_with(NO_AUTH_WARNING));
        assert!(l[0].1.ends_with("1 union mesh, 1 mesh, 1 image skipped."), "{}", l[0].1);
    }

    #[test]
    fn nothing_missing_logs_nothing() {
        let out = SharedOutput::default();
        let mut d = AssetDownloads::default();
        assert!(!d.begin(Vec::new(), None, None, &out, |_| -> Arc<dyn Fetch> { unreachable!() }));
        assert!(lines(&out).is_empty());
    }

    #[test]
    fn downloads_into_cache_and_skips_inaccessible() {
        let dir = std::env::temp_dir().join(format!("phaedrus-asset-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let out = SharedOutput::default();
        let mut d = AssetDownloads::default();
        let key = ApiKey::new("test-key").unwrap();
        let started = d.begin(vec![(10, AssetKind::UnionMesh), (11, AssetKind::UnionMesh), (12, AssetKind::UnionMesh)], Some(&key), Some(&dir), &out, |_| {
            Arc::new(Fake(|id| if id == 11 { Err(ApiError::Forbidden) } else { Ok(vec![id as u8]) }))
        });
        assert!(started);
        wait(&d);
        assert_eq!(std::fs::read(cache_file(&dir, 10, AssetKind::UnionMesh)).unwrap(), [10]);
        assert!(!cache_file(&dir, 11, AssetKind::UnionMesh).exists());
        let mut arrived: Vec<u64> = d.take_arrivals().iter().map(|a| a.id).collect();
        arrived.sort_unstable();
        assert_eq!(arrived, [10, 12], "only downloaded assets arrive");
        assert!(d.take_arrivals().is_empty(), "arrivals are handed over once");
        assert!(lines(&out).iter().any(|(_, t)| t.contains("2 downloaded, 1 not accessible")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unauthorized_stops_the_queue_without_leaking_the_key() {
        let dir = std::env::temp_dir().join(format!("phaedrus-asset-test-401-{}", std::process::id()));
        let out = SharedOutput::default();
        let mut d = AssetDownloads::default();
        let key = ApiKey::new("SECRET-KEY").unwrap();
        d.begin((1..=50).map(|id| (id, AssetKind::Mesh)).collect(), Some(&key), Some(&dir), &out, |_| Arc::new(Fake(|_| Err(ApiError::Unauthorized))));
        wait(&d);
        let l = lines(&out);
        assert_eq!(l.iter().filter(|(lvl, _)| *lvl == OutputLevel::Error).count(), 1);
        assert!(l.iter().all(|(_, t)| !t.contains("SECRET")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn images_are_sniffed_and_decals_followed() {
        use phaedrus_dom::{InstanceBuilder, WeakDom};
        const PNG: &[u8] = b"\x89PNG\r\n\x1a\n....";
        fn decal_model() -> Vec<u8> {
            let dom = WeakDom::new(InstanceBuilder::new("DataModel").with_child(
                InstanceBuilder::new("Decal").with_property("Texture", phaedrus_dom::types::ContentId::from("rbxassetid://21")),
            ));
            let mut out = Vec::new();
            rbx_binary::to_writer(&mut out, &dom, dom.root().children()).unwrap();
            out
        }
        let dir = std::env::temp_dir().join(format!("phaedrus-asset-test-img-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let out = SharedOutput::default();
        let mut d = AssetDownloads::default();
        let key = ApiKey::new("k").unwrap();
        let ids = vec![(20, AssetKind::Image), (22, AssetKind::Image), (23, AssetKind::Mesh)];
        d.begin(ids, Some(&key), Some(&dir), &out, |_| {
            Arc::new(Fake(|id| match id {
                20 => Ok(decal_model()),
                21 => Ok(PNG.to_vec()),
                22 => Ok(b"RIFF....WEBP".to_vec()),
                _ => Ok(quad_mesh()),
            }))
        });
        wait(&d);
        // The Decal's image is cached under the Decal's own id.
        assert_eq!(std::fs::read(dir.join("20.png")).unwrap(), PNG);
        assert!(phaedrus_dom::content::cached_image(&dir, 22).is_none());
        assert_eq!(std::fs::read(cache_file(&dir, 23, AssetKind::Mesh)).unwrap(), quad_mesh());
        // Meshes arrive already decoded, so the main thread does no parsing.
        let arrivals = d.take_arrivals();
        let mesh = arrivals.iter().find(|a| a.id == 23).expect("mesh arrived");
        assert!(matches!(&mesh.decoded, Some(Decoded::Mesh(m)) if m.indices.len() == 6));
        assert!(lines(&out).iter().any(|(_, t)| t.contains("2 downloaded, 0 not accessible with this key, 1 in unsupported formats")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A version 2.00 mesh holding one quad.
    fn quad_mesh() -> Vec<u8> {
        let mut b = b"version 2.00\n".to_vec();
        b.extend_from_slice(&12u16.to_le_bytes());
        b.extend_from_slice(&[36, 12]);
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(&2u32.to_le_bytes());
        for (x, y) in [(0.0f32, 0.0f32), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
            for f in [x, y, 0.0, 0.0, 0.0, 1.0, x, y] {
                b.extend_from_slice(&f.to_le_bytes());
            }
            b.extend_from_slice(&[0; 4]);
        }
        for i in [0u32, 1, 2, 0, 2, 3] {
            b.extend_from_slice(&i.to_le_bytes());
        }
        b
    }
}
