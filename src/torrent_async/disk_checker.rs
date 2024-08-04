use std::path::PathBuf;

use walkdir::{DirEntry, WalkDir};

use super::models::{BlockStatus, File, Torrent};

#[derive(Debug)]
pub(super) struct DiskChecker {
    temp_path: PathBuf,
    dest_path: PathBuf,
    temp_path_entries: Vec<DirEntry>,
    dest_path_entries: Vec<DirEntry>,
}

#[derive(Debug)]
pub(super) enum Event {
    BlockPersistedSeparately(String, PathBuf),
    FilePersistedPermanently(usize, PathBuf),
}

impl DiskChecker {
    pub(super) fn new(temp_path: PathBuf, dest_path: PathBuf) -> anyhow::Result<Self> {
        Ok(Self {
            temp_path_entries: WalkDir::new(temp_path.as_path())
                .into_iter()
                .filter_map(|e| e.ok())
                .collect::<Vec<DirEntry>>(),
            dest_path_entries: WalkDir::new(dest_path.as_path())
                .into_iter()
                .filter_map(|e| e.ok())
                .collect::<Vec<DirEntry>>(),
            temp_path,
            dest_path,
        })
    }

    pub(super) async fn sync_with_disk(
        &mut self,
        torrent: &Torrent,
    ) -> anyhow::Result<Option<Event>> {
        Ok(self
            .sync_temp_blocks(torrent)?
            .or(self.sync_complete_files(torrent)?))
    }

    fn sync_temp_blocks(&mut self, torrent: &Torrent) -> anyhow::Result<Option<Event>> {
        let mut persisted_block_info = None;
        for (index, entry) in self.temp_path_entries.iter().enumerate() {
            let entry = entry.path();
            let file_name = entry.file_name().unwrap().to_str().unwrap();
            if let Some(block) = torrent.blocks.get(file_name) {
                if !matches!(
                    block.data_status,
                    BlockStatus::PersistedSeparately(_) | BlockStatus::PersistedInFile
                ) {
                    persisted_block_info =
                        Some((file_name.to_string(), entry.to_path_buf(), index));
                    break;
                }
            }
        }
        Ok(persisted_block_info.map(|(block_id, path, _index)| {
            // TODO: Decide if we should do this.
            // self.temp_path_entries.remove(_index);
            Event::BlockPersistedSeparately(block_id, path)
        }))
    }

    fn sync_complete_files(&mut self, torrent: &Torrent) -> anyhow::Result<Option<Event>> {
        let mut completed_file_info = None;
        for (index, entry) in self.dest_path_entries.iter().enumerate() {
            let entry = entry.path();
            let matching_files = torrent
                .files
                .iter()
                .filter(|f| self.dest_path.join(f.relative_path.as_path()) == entry)
                .collect::<Vec<&File>>();
            if matching_files.len() == 1 {
                let file = matching_files[0];
                if file.path.is_none() && file.length as u64 == entry.metadata().unwrap().len() {
                    completed_file_info = Some((file.index, entry.to_path_buf(), index));
                    break;
                }
            }
        }
        Ok(completed_file_info.map(|(file_index, path, _index)| {
            // TODO: Decide if we should do this.
            // self.dest_path_entries.remove(_index);
            Event::FilePersistedPermanently(file_index, path)
        }))
    }
}
