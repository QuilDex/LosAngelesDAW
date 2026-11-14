//! Sound packs (folders of samples) and plugin discovery for the browser panel.

use std::fs;
use std::path::{Path, PathBuf};

const AUDIO_EXTS: [&str; 6] = ["wav", "mp3", "ogg", "flac", "aif", "aiff"];

fn ext_lower(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

pub fn is_audio(path: &Path) -> bool {
    AUDIO_EXTS.contains(&ext_lower(path).as_str())
}

#[derive(Clone)]
pub struct SoundFile {
    pub name: String,
    pub path: PathBuf,
}

/// A pack is just a folder of samples (nested sub-folders are included).
#[derive(Clone)]
pub struct Pack {
    pub name: String,
    pub root: PathBuf,
    pub sounds: Vec<SoundFile>,
}

impl Pack {
    pub fn scan(root: PathBuf) -> Self {
        let name = root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pack")
            .to_string();
        let mut sounds = Vec::new();
        collect_sounds(&root, &mut sounds, 0);
        sounds.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Self {
            name,
            root,
            sounds,
        }
    }
}

fn collect_sounds(dir: &Path, out: &mut Vec<SoundFile>, depth: u8) {
    if depth > 4 || out.len() > 4000 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sounds(&path, out, depth + 1);
        } else if is_audio(&path) {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("sample")
                .to_string();
            out.push(SoundFile { name, path });
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PluginKind {
    Clap,
    Vst3,
    Vst,
}

impl PluginKind {
    pub fn label(self) -> &'static str {
        match self {
            PluginKind::Clap => "CLAP",
            PluginKind::Vst3 => "VST3",
            PluginKind::Vst => "VST",
        }
    }
}

#[derive(Clone)]
pub struct PluginEntry {
    pub name: String,
    pub path: PathBuf,
    pub kind: PluginKind,
}

impl PluginEntry {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = ext_lower(path);
        let kind = match ext.as_str() {
            "clap" => PluginKind::Clap,
            "vst3" => PluginKind::Vst3,
            "vst" => PluginKind::Vst,
            _ => return None,
        };
        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("plugin")
            .to_string();
        Some(Self {
            name,
            path: path.to_path_buf(),
            kind,
        })
    }
}

/// Recursively find .clap / .vst3 / .vst entries below `dir`.
pub fn scan_plugins(dir: &Path, out: &mut Vec<PluginEntry>) {
    scan_plugins_depth(dir, out, 0);
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out.dedup_by(|a, b| a.path == b.path);
}

fn scan_plugins_depth(dir: &Path, out: &mut Vec<PluginEntry>, depth: u8) {
    if depth > 3 || out.len() > 4000 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = ext_lower(&path);
        // A .vst3 / .clap bundle is a directory on macOS/Linux: keep it whole.
        if path.is_dir() {
            if ext == "vst3" || ext == "clap" {
                if let Some(entry) = PluginEntry::from_path(&path) {
                    out.push(entry);
                }
            } else {
                scan_plugins_depth(&path, out, depth + 1);
            }
        } else if let Some(entry) = PluginEntry::from_path(&path) {
            out.push(entry);
        }
    }
}

/// Usual system locations, so the browser has something to show on first run.
pub fn default_plugin_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        if let Ok(pf) = std::env::var("ProgramFiles") {
            dirs.push(PathBuf::from(&pf).join("Common Files").join("VST3"));
            dirs.push(PathBuf::from(&pf).join("Common Files").join("CLAP"));
            dirs.push(PathBuf::from(&pf).join("VSTPlugins"));
            dirs.push(PathBuf::from(&pf).join("Steinberg").join("VSTPlugins"));
        }
        if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
            dirs.push(PathBuf::from(&pf86).join("VSTPlugins"));
            dirs.push(PathBuf::from(&pf86).join("Steinberg").join("VSTPlugins"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
        dirs.push(PathBuf::from("/Library/Audio/Plug-Ins/CLAP"));
        dirs.push(PathBuf::from("/Library/Audio/Plug-Ins/VST"));
    } else {
        dirs.push(PathBuf::from("/usr/lib/vst3"));
        dirs.push(PathBuf::from("/usr/lib/clap"));
        if let Ok(home) = std::env::var("HOME") {
            dirs.push(PathBuf::from(&home).join(".vst3"));
            dirs.push(PathBuf::from(&home).join(".clap"));
        }
    }
    dirs
}
