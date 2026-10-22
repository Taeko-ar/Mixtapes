use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::tag::ItemKey;
use sha1::{Digest, Sha1};

use crate::model::{Named, Person, Track, VideoId};
use crate::paths::Paths;

pub const PREFIX: &str = "FILE_";
pub const PREF: &str = "local_folder";
pub const PAGE_ID: &str = "LOCALFILES";
const EXTENSIONS: [&str; 9] = ["mp3", "flac", "ogg", "oga", "opus", "m4a", "aac", "wav", "wma"];

static FILES: LazyLock<RwLock<HashMap<String, PathBuf>>> = LazyLock::new(|| RwLock::new(HashMap::new()));
static TRACKS: RwLock<Vec<Track>> = RwLock::new(Vec::new());

pub fn is_file_id(id: &str) -> bool {
    id.starts_with(PREFIX)
}

pub fn path(id: &str) -> Option<PathBuf> {
    FILES.read().unwrap().get(id).filter(|p| p.is_file()).cloned()
}

pub fn known() -> Vec<Track> {
    TRACKS.read().unwrap().clone()
}

pub fn folder(paths: &Paths) -> Option<PathBuf> {
    paths.read_prefs().get(PREF).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(PathBuf::from)
}

fn id_for(path: &Path) -> String {
    let digest = Sha1::digest(path.to_string_lossy().as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("{PREFIX}{hex}")
}

pub fn scan(folder: &Path, cache_dir: &Path) -> Vec<Track> {
    let covers = cache_dir.join("local_covers");
    let _ = std::fs::create_dir_all(&covers);
    let mut found: Vec<(Track, PathBuf, u32)> = Vec::new();
    let mut dirs = vec![folder.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                dirs.push(path);
            } else if is_audio(&path) {
                let (track, number) = read_track(&path, &covers);
                found.push((track, path, number));
            }
        }
    }
    found.sort_by(|(a, _, an), (b, _, bn)| {
        let album = |t: &Track| t.album.as_ref().map(|a| a.name.to_lowercase()).unwrap_or_default();
        (a.artist.to_lowercase(), album(a), *an, a.title.to_lowercase()).cmp(&(b.artist.to_lowercase(), album(b), *bn, b.title.to_lowercase()))
    });
    let mut map = HashMap::with_capacity(found.len());
    let tracks: Vec<Track> = found
        .into_iter()
        .map(|(track, path, _)| {
            map.insert(track.video_id.0.clone(), path);
            track
        })
        .collect();
    *FILES.write().unwrap() = map;
    *TRACKS.write().unwrap() = tracks.clone();
    tracks
}

fn is_audio(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn read_track(path: &Path, covers: &Path) -> (Track, u32) {
    let id = id_for(path);
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut track = Track { video_id: VideoId(id.clone()), title: stem, ..Track::default() };
    let Ok(file) = lofty::read_from_path(path) else { return (track, 0) };
    let seconds = file.properties().duration().as_secs();
    track.duration_seconds = (seconds > 0).then_some(seconds as u32);
    let Some(tag) = file.primary_tag().or_else(|| file.first_tag()) else { return (track, 0) };
    let text = |key| tag.get_string(key).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    if let Some(title) = text(ItemKey::TrackTitle) {
        track.title = title;
    }
    if let Some(artist) = text(ItemKey::TrackArtist).or_else(|| text(ItemKey::AlbumArtist)) {
        track.artists = vec![Person { name: artist.clone(), id: None }];
        track.artist = artist;
    }
    track.album = text(ItemKey::AlbumTitle).map(|name| Named { name, id: None });
    let number = text(ItemKey::TrackNumber).and_then(|n| n.split('/').next()?.trim().parse().ok()).unwrap_or(0);

    let cover = covers.join(format!("{id}.jpg"));
    if !cover.is_file() {
        let picture = tag.pictures().iter().find(|p| p.pic_type() == PictureType::CoverFront).or_else(|| tag.pictures().first());
        if let Some(picture) = picture {
            let _ = std::fs::write(&cover, picture.data());
        }
    }
    if cover.is_file() {
        track.thumb = glib::filename_to_uri(&cover, None).ok().map(|u| u.to_string());
    }
    (track, number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_marked() {
        let id = id_for(Path::new("/music/a/song.flac"));
        assert!(is_file_id(&id));
        assert_eq!(id, id_for(Path::new("/music/a/song.flac")));
        assert_ne!(id, id_for(Path::new("/music/b/song.flac")));
        assert_ne!(id.len(), 11);
    }

    #[test]
    fn scan_reads_tags_and_maps_ids_back() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("Artist");
        std::fs::create_dir_all(&sub).unwrap();
        let tagged = sub.join("tagged.flac");
        let bare = dir.path().join("bare.flac");
        for (path, title) in [(&tagged, Some("Song")), (&bare, None)] {
            let mut cmd = std::process::Command::new("ffmpeg");
            cmd.args(["-y", "-f", "lavfi", "-i", "anullsrc=r=48000:cl=mono", "-t", "1"]);
            if let Some(title) = title {
                cmd.args(["-metadata", &format!("title={title}"), "-metadata", "artist=Artist"]);
            }
            let made = cmd.arg(path).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
            if !made.map(|s| s.success()).unwrap_or(false) {
                println!("ffmpeg missing, skipping");
                return;
            }
        }
        std::fs::write(dir.path().join("notes.txt"), "not audio").unwrap();

        let cache = tempfile::tempdir().unwrap();
        let tracks = scan(dir.path(), cache.path());
        assert_eq!(tracks.len(), 2);
        let song = tracks.iter().find(|t| t.title == "Song").expect("tagged file");
        assert_eq!(song.artist, "Artist");
        assert_eq!(song.duration_seconds, Some(1));
        assert!(tracks.iter().any(|t| t.title == "bare"));
        assert_eq!(path(&song.video_id.0), Some(tagged));
    }
}
