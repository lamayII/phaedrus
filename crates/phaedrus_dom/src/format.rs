//! Reading and writing Roblox place/model files.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use rbx_dom_weak::WeakDom;
use rbx_types::Ref;

/// The on-disk encodings Roblox uses for places and models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    /// `.rbxl` / `.rbxm`
    Binary,
    /// `.rbxlx` / `.rbxmx`
    Xml,
}

impl FileFormat {
    /// Guesses the format from a file extension.
    pub fn from_extension(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "rbxl" | "rbxm" => Some(Self::Binary),
            "rbxlx" | "rbxmx" => Some(Self::Xml),
            _ => None,
        }
    }

    /// Detects the format from the first bytes of the file. Binary files start
    /// with `<roblox!`, XML files start with `<roblox ` (possibly after a BOM
    /// or whitespace).
    pub fn sniff(header: &[u8]) -> Option<Self> {
        if header.starts_with(b"<roblox!") {
            return Some(Self::Binary);
        }
        let trimmed = header
            .strip_prefix(&[0xEF, 0xBB, 0xBF])
            .unwrap_or(header)
            .trim_ascii_start();
        if trimmed.starts_with(b"<roblox") || trimmed.starts_with(b"<?xml") {
            return Some(Self::Xml);
        }
        None
    }
}

/// Whether a path names a model file (`.rbxm`/`.rbxmx`) rather than a place.
pub fn is_model_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()),
        Some(ref e) if e == "rbxm" || e == "rbxmx"
    )
}

/// Loads a place or model file. The format is sniffed from the file contents,
/// so a mis-named file still loads.
pub fn load_file(path: &Path) -> Result<WeakDom> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut header = [0u8; 16];
    let n = file.read(&mut header)?;
    file.seek(SeekFrom::Start(0))?;
    let format = FileFormat::sniff(&header[..n])
        .or_else(|| FileFormat::from_extension(path))
        .context("unrecognised Roblox file format")?;
    load_reader(BufReader::new(file), format)
}

/// Loads a DOM from any reader in the given format.
pub fn load_reader<R: Read>(reader: R, format: FileFormat) -> Result<WeakDom> {
    Ok(match format {
        FileFormat::Binary => rbx_binary::from_reader(reader)?,
        FileFormat::Xml => rbx_xml::from_reader_default(reader)?,
    })
}

/// Saves a DOM. For places, every child of the root (the services) is
/// written; for models, pass the instances to serialize in `refs`.
pub fn save_file(path: &Path, dom: &WeakDom, refs: Option<&[Ref]>) -> Result<()> {
    let Some(format) = FileFormat::from_extension(path) else {
        bail!("unsupported extension for {}", path.display());
    };
    let refs: Vec<Ref> = match refs {
        Some(r) => r.to_vec(),
        None => dom.root().children().to_vec(),
    };
    // Write to a temp file first so a failed save never corrupts the original.
    let tmp = path.with_extension("phaedrus-tmp");
    {
        let mut writer = BufWriter::new(File::create(&tmp)?);
        match format {
            FileFormat::Binary => rbx_binary::to_writer(&mut writer, dom, &refs)?,
            FileFormat::Xml => rbx_xml::to_writer_default(&mut writer, dom, &refs)?,
        }
        writer.flush()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Deep-copies a whole DOM by round-tripping it through the binary format.
/// Referents change; the copy is fully independent.
pub fn clone_dom(dom: &WeakDom) -> Result<WeakDom> {
    let mut bytes = Vec::new();
    rbx_binary::to_writer(&mut bytes, dom, dom.root().children())?;
    Ok(rbx_binary::from_reader(&bytes[..])?)
}
