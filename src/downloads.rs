use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::Write,
    path::PathBuf,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub use crate::download::Receipt;
use crate::music::{Provider, Song};

const COMPANY: &str = "song-download";
const HISTORY: &str = "downloads.json";

/// Identity of a track inside one provider: QQ uses the song mid, NetEase the
/// numeric id.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TrackKey {
    pub provider: Provider,
    pub id: String,
}

impl TrackKey {
    pub fn new(provider: Provider, song: &Song) -> Self {
        let id = match provider {
            Provider::Qq => &song.mid,
            Provider::Netease => &song.id,
        };
        Self {
            provider,
            id: id.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Queued,
    Running,
    Completed(Receipt),
    Failed(String),
}

/// One row of the download manager. Completed rows restored from disk keep
/// only their receipt; queued/failed rows additionally remember their song and
/// target folder in the queue itself.
#[derive(Clone, Debug)]
pub struct Entry {
    pub key: TrackKey,
    pub provider: Provider,
    pub title: String,
    pub artist: String,
    pub status: Status,
}

/// Snapshot handed to a worker when a queued job starts running.
pub struct Job {
    pub key: TrackKey,
    pub provider: Provider,
    pub song: Song,
    pub folder: PathBuf,
}

struct Source {
    provider: Provider,
    song: Song,
    folder: PathBuf,
}

pub struct Queue {
    entries: Vec<Entry>,
    index: HashMap<TrackKey, usize>,
    sources: HashMap<TrackKey, Source>,
    waiting: VecDeque<TrackKey>,
    running: Option<TrackKey>,
    history: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct History {
    #[serde(default)]
    completed: Vec<Record>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    key: TrackKey,
    title: String,
    artist: String,
    audio_path: PathBuf,
    summary: String,
}

impl Queue {
    /// In-memory queue without history, for previews and tests.
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::new(),
            sources: HashMap::new(),
            waiting: VecDeque::new(),
            running: None,
            history: None,
        }
    }

    /// Loads completed downloads; any problem is reported to the UI while the
    /// queue itself starts from a usable (possibly empty) state.
    pub fn open() -> (Self, Option<String>) {
        let path = dirs::data_local_dir().map(|dir| dir.join(COMPANY).join(HISTORY));
        Self::load(path)
    }

    fn load(path: Option<PathBuf>) -> (Self, Option<String>) {
        let mut queue = Self {
            history: path,
            ..Self::empty()
        };
        let Some(path) = queue.history.clone() else {
            return (
                queue,
                Some("无法定位应用数据目录，下载记录不会被保存".to_owned()),
            );
        };
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (queue, None),
            Err(error) => {
                return (
                    queue,
                    Some(format!("无法读取下载记录 {}：{error}", path.display())),
                );
            }
        };
        let history: History = match serde_json::from_slice(&bytes) {
            Ok(history) => history,
            Err(error) => {
                return (queue, Some(format!("下载记录已损坏，已忽略（{error}）")));
            }
        };

        let mut missing = 0usize;
        for record in history.completed {
            if !record.audio_path.is_file() {
                missing += 1;
                continue;
            }
            if queue.index.contains_key(&record.key) {
                continue;
            }
            let provider = record.key.provider;
            queue.index.insert(record.key.clone(), queue.entries.len());
            queue.entries.push(Entry {
                key: record.key,
                provider,
                title: record.title,
                artist: record.artist,
                status: Status::Completed(Receipt {
                    audio_path: record.audio_path,
                    summary: record.summary,
                }),
            });
        }
        let error = (missing > 0).then(|| format!("已清理 {missing} 条文件不存在的下载记录"));
        (queue, error)
    }

    pub fn status(&self, key: &TrackKey) -> Option<&Status> {
        self.index
            .get(key)
            .map(|index| &self.entries[*index].status)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Starts a new task, or restarts a failed one. Existing queued, running or
    /// completed tracks are rejected.
    pub fn enqueue(&mut self, provider: Provider, song: Song, folder: PathBuf) -> bool {
        let key = TrackKey::new(provider, &song);
        if let Some(&index) = self.index.get(&key) {
            if !matches!(self.entries[index].status, Status::Failed(_)) {
                return false;
            }
            self.entries[index].status = Status::Queued;
            self.entries[index].title = song.name.clone();
            self.entries[index].artist = song.artist.clone();
            self.waiting.push_back(key.clone());
            self.sources.insert(
                key,
                Source {
                    provider,
                    song,
                    folder,
                },
            );
            return true;
        }

        self.index.insert(key.clone(), self.entries.len());
        self.entries.push(Entry {
            key: key.clone(),
            provider,
            title: song.name.clone(),
            artist: song.artist.clone(),
            status: Status::Queued,
        });
        self.waiting.push_back(key.clone());
        self.sources.insert(
            key,
            Source {
                provider,
                song,
                folder,
            },
        );
        true
    }

    /// Makes the oldest queued track running. A second concurrent job is never
    /// started while one is still running.
    pub fn next(&mut self) -> Option<Job> {
        if self.running.is_some() {
            return None;
        }
        let key = self.waiting.pop_front()?;
        let index = self.index[&key];
        let Some(source) = self.sources.get(&key) else {
            self.entries[index].status = Status::Failed("任务信息丢失".to_owned());
            return None;
        };
        let job = Job {
            key: key.clone(),
            provider: source.provider,
            song: source.song.clone(),
            folder: source.folder.clone(),
        };
        self.entries[index].status = Status::Running;
        self.running = Some(key);
        Some(job)
    }

    /// Removes a waiting task. Running and finished tasks cannot be cancelled.
    pub fn cancel(&mut self, key: &TrackKey) -> bool {
        let Some(index) = self.index.get(key).copied() else {
            return false;
        };
        if !matches!(self.entries[index].status, Status::Queued) {
            return false;
        }
        self.entries.remove(index);
        self.sources.remove(key);
        self.waiting.retain(|waiting| waiting != key);
        self.reindex();
        true
    }

    /// Requeues a failed task using the song and folder remembered in-session.
    pub fn retry(&mut self, key: &TrackKey) -> bool {
        let Some(index) = self.index.get(key).copied() else {
            return false;
        };
        if !matches!(self.entries[index].status, Status::Failed(_))
            || !self.sources.contains_key(key)
        {
            return false;
        }
        self.entries[index].status = Status::Queued;
        self.waiting.push_back(key.clone());
        true
    }

    /// Records the outcome of the running job. Results for any other track are
    /// ignored. Successful downloads are persisted atomically.
    pub fn finish(&mut self, key: &TrackKey, result: Result<Receipt>) -> Result<()> {
        if self.running.as_ref() != Some(key) {
            return Ok(());
        }
        self.running = None;
        let Some(index) = self.index.get(key).copied() else {
            return Ok(());
        };
        match result {
            Ok(receipt) => {
                self.entries[index].status = Status::Completed(receipt);
                self.sources.remove(key);
                self.persist()
            }
            Err(error) => {
                self.entries[index].status = Status::Failed(format!("{error:#}"));
                Ok(())
            }
        }
    }

    /// Tasks that have not finished yet (waiting or currently downloading).
    pub fn pending_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.status, Status::Queued | Status::Running))
            .count()
    }

    fn reindex(&mut self) {
        self.index.clear();
        for (index, entry) in self.entries.iter().enumerate() {
            self.index.insert(entry.key.clone(), index);
        }
    }

    fn persist(&self) -> Result<()> {
        let Some(path) = &self.history else {
            return Ok(());
        };
        let history = History {
            completed: self
                .entries
                .iter()
                .filter_map(|entry| match &entry.status {
                    Status::Completed(receipt) => Some(Record {
                        key: entry.key.clone(),
                        title: entry.title.clone(),
                        artist: entry.artist.clone(),
                        audio_path: receipt.audio_path.clone(),
                        summary: receipt.summary.clone(),
                    }),
                    _ => None,
                })
                .collect(),
        };
        let data = serde_json::to_vec_pretty(&history).context("无法序列化下载记录")?;
        let dir = path.parent().context("下载记录目录无效")?;
        fs::create_dir_all(dir)
            .with_context(|| format!("无法创建下载记录目录 {}", dir.display()))?;
        let mut temp = tempfile::NamedTempFile::new_in(dir).context("无法创建下载记录临时文件")?;
        temp.write_all(&data).context("无法写入下载记录")?;
        temp.persist(path)
            .with_context(|| format!("无法保存下载记录 {}", path.display()))?;
        Ok(())
    }
}

impl Default for Queue {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn song(mid: &str, id: &str, name: &str) -> Song {
        Song {
            id: id.to_owned(),
            mid: mid.to_owned(),
            media_mid: mid.to_owned(),
            name: name.to_owned(),
            artist: "歌手".to_owned(),
            album: "专辑".to_owned(),
            cover: None,
            seconds: 200,
        }
    }

    fn receipt(folder: &Path, name: &str) -> Receipt {
        let audio_path = folder.join(name);
        fs::write(&audio_path, b"audio").unwrap();
        Receipt {
            audio_path,
            summary: format!("已保存 {name}"),
        }
    }

    fn key(provider: Provider, mid: &str, id: &str) -> TrackKey {
        TrackKey::new(provider, &song(mid, id, "x"))
    }

    #[test]
    fn runs_submissions_one_at_a_time_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let (mut queue, error) = Queue::load(Some(dir.path().join("downloads.json")));
        assert!(error.is_none());

        assert!(queue.enqueue(Provider::Qq, song("m1", "1", "第一首"), dir.path().into()));
        assert!(queue.enqueue(Provider::Qq, song("m2", "2", "第二首"), dir.path().into()));
        assert_eq!(queue.pending_count(), 2);

        let job = queue.next().unwrap();
        assert_eq!(job.key, key(Provider::Qq, "m1", "1"));
        assert!(queue.next().is_none(), "不能同时开始第二个任务");
        assert_eq!(queue.pending_count(), 2);

        let path = receipt(dir.path(), "第一首.mp3");
        queue.finish(&job.key, Ok(path.clone())).unwrap();
        assert!(matches!(
            queue.status(&job.key),
            Some(Status::Completed(saved)) if saved.audio_path == path.audio_path
        ));
        assert_eq!(queue.pending_count(), 1);

        let second = queue.next().unwrap();
        assert_eq!(second.key, key(Provider::Qq, "m2", "2"));
    }

    #[test]
    fn rejects_duplicates_and_separates_providers() {
        let dir = tempfile::tempdir().unwrap();
        let (mut queue, _) = Queue::load(Some(dir.path().join("downloads.json")));

        assert!(queue.enqueue(Provider::Qq, song("same", "1", "歌"), dir.path().into()));
        assert!(!queue.enqueue(Provider::Qq, song("same", "1", "歌"), dir.path().into()));
        assert!(queue.enqueue(
            Provider::Netease,
            song("same", "1", "歌"),
            dir.path().into()
        ));
        assert_eq!(queue.entries().len(), 2);
        assert!(queue.status(&key(Provider::Qq, "same", "1")).is_some());
        assert!(queue.status(&key(Provider::Netease, "same", "1")).is_some());
        assert!(queue.status(&key(Provider::Qq, "other", "1")).is_none());
    }

    #[test]
    fn retries_failures_and_cancels_only_waiting_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let (mut queue, _) = Queue::load(Some(dir.path().join("downloads.json")));
        let track = |name: &str| song(name, name, name);

        queue.enqueue(Provider::Qq, track("a"), dir.path().into());
        queue.enqueue(Provider::Qq, track("b"), dir.path().into());
        let job = queue.next().unwrap();
        queue
            .finish(&job.key, Err(anyhow::anyhow!("网络断开")))
            .unwrap();
        assert!(matches!(queue.status(&job.key), Some(Status::Failed(_))));
        assert!(!queue.cancel(&job.key), "运行过的任务不能取消");
        assert!(queue.retry(&job.key));
        assert!(matches!(queue.status(&job.key), Some(Status::Queued)));

        let waiting = key(Provider::Qq, "b", "b");
        assert!(queue.cancel(&waiting));
        assert!(queue.status(&waiting).is_none());
        assert_eq!(queue.pending_count(), 1);

        // A cancelled track can be submitted again.
        assert!(queue.enqueue(Provider::Qq, track("b"), dir.path().into()));

        // Fail the retried track, then resubmit it through enqueue.
        let job = queue.next().unwrap();
        assert_eq!(job.key, key(Provider::Qq, "a", "a"));
        queue
            .finish(&job.key, Err(anyhow::anyhow!("再次失败")))
            .unwrap();
        assert!(queue.enqueue(Provider::Qq, track("a"), dir.path().into()));
        assert_eq!(queue.entries().len(), 2);
        assert!(matches!(queue.status(&job.key), Some(Status::Queued)));
    }

    #[test]
    fn retry_goes_behind_an_already_waiting_track() {
        let (mut queue, _) = Queue::load(None);
        let folder = PathBuf::from("/unused");
        queue.enqueue(Provider::Qq, song("a", "a", "A"), folder.clone());
        queue.enqueue(Provider::Qq, song("b", "b", "B"), folder);
        let first = queue.next().unwrap();
        queue
            .finish(&first.key, Err(anyhow::anyhow!("下载失败")))
            .unwrap();
        assert!(queue.retry(&first.key));
        assert_eq!(queue.next().unwrap().key.id, "b");
    }

    #[test]
    fn restores_history_and_drops_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("downloads.json");
        let track = song("m1", "1", "留下的歌");

        {
            let (mut queue, _) = Queue::load(Some(path.clone()));
            queue.enqueue(Provider::Qq, track.clone(), dir.path().into());
            let job = queue.next().unwrap();
            queue
                .finish(&job.key, Ok(receipt(dir.path(), "留下的歌.mp3")))
                .unwrap();

            queue.enqueue(
                Provider::Netease,
                song("m2", "2", "删除的歌"),
                dir.path().into(),
            );
            let job = queue.next().unwrap();
            queue
                .finish(&job.key, Ok(receipt(dir.path(), "删除的歌.mp3")))
                .unwrap();
        }

        let saved = dir.path().join("留下的歌.mp3");
        let removed = dir.path().join("删除的歌.mp3");
        assert!(saved.is_file() && removed.is_file());

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("audio_path"));
        assert!(!text.to_ascii_lowercase().contains("cookie"));
        assert!(!text.contains("Queued"));

        fs::remove_file(&removed).unwrap();
        let (mut queue, error) = Queue::load(Some(path));
        assert_eq!(queue.entries().len(), 1);
        assert_eq!(queue.entries()[0].title, "留下的歌");
        assert!(matches!(queue.entries()[0].status, Status::Completed(_)));
        assert!(error.unwrap().contains('1'));

        // A restored track is not downloaded again.
        assert!(!queue.enqueue(Provider::Qq, track, dir.path().into()));
    }

    #[test]
    fn starts_empty_when_history_is_corrupted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("downloads.json");
        fs::write(&path, b"{ not json").unwrap();

        let (queue, error) = Queue::load(Some(path.clone()));
        assert!(queue.entries().is_empty());
        assert!(error.unwrap().contains("损坏"));

        // A fresh write replaces the corrupted file.
        let (mut queue, _) = Queue::load(Some(path.clone()));
        queue.enqueue(Provider::Qq, song("m1", "1", "歌"), dir.path().into());
        let job = queue.next().unwrap();
        queue
            .finish(&job.key, Ok(receipt(dir.path(), "歌.mp3")))
            .unwrap();
        let restored = serde_json::from_slice::<History>(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(restored.completed.len(), 1);
    }

    #[test]
    fn ignores_results_from_other_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let (mut queue, _) = Queue::load(Some(dir.path().join("downloads.json")));
        queue.enqueue(Provider::Qq, song("m1", "1", "第一首"), dir.path().into());
        queue.enqueue(Provider::Qq, song("m2", "2", "第二首"), dir.path().into());
        let job = queue.next().unwrap();

        let stray = key(Provider::Qq, "m2", "2");
        queue
            .finish(&stray, Ok(receipt(dir.path(), "错.mp3")))
            .unwrap();
        assert!(matches!(queue.status(&stray), Some(Status::Queued)));
        queue
            .finish(&job.key, Ok(receipt(dir.path(), "对.mp3")))
            .unwrap();
        assert!(matches!(queue.status(&job.key), Some(Status::Completed(_))));
    }
}
