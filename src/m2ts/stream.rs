use crate::error::PgsError;
use crate::io::SeekBufReader;
use crate::pgs::DisplaySetAssembler;
use crate::{ContainerFormat, PgsTrackInfo, TrackDisplaySet};
use super::pes::PesReassembler;
use super::ts_packet::{self, PacketFormat};
use super::{M2tsMetadata, find_sync_start, SCAN_BLOCK_SIZE, MAX_RESYNC_SCAN};
use std::collections::{HashMap, VecDeque};
use std::fs::File;

/// Streaming M2TS extractor state machine.
///
/// Yields `TrackDisplaySet` one at a time by scanning TS packets in 2 MB blocks
/// and pushing PGS data through PES reassemblers and display set assemblers.
pub(crate) struct M2tsExtractorState {
    reader: SeekBufReader<File>,
    format: PacketFormat,
    pid_state: HashMap<u16, (PesReassembler, DisplaySetAssembler)>,
    track_info: HashMap<u16, PgsTrackInfo>,
    scan_end: u64,
    pending: VecDeque<TrackDisplaySet>,
    flushed: bool,
    container: ContainerFormat,
}

impl M2tsExtractorState {
    /// Create a new M2TS extractor from pre-parsed metadata.
    ///
    /// The `track_filter` restricts extraction to specific PIDs.
    /// If `None`, all PGS tracks are extracted.
    pub(crate) fn new(
        reader: SeekBufReader<File>,
        metadata: M2tsMetadata,
        container: ContainerFormat,
        track_filter: Option<&[u32]>,
    ) -> Self {
        // Determine which PIDs to extract.
        let active_pids: Vec<u16> = if let Some(filter) = track_filter {
            metadata.pgs_pids.iter()
                .filter(|&&pid| filter.contains(&(pid as u32)))
                .copied()
                .collect()
        } else {
            metadata.pgs_pids.clone()
        };

        // Build track info map.
        let mut track_info = HashMap::new();
        for t in &metadata.tracks {
            if active_pids.contains(&t.pid) {
                track_info.insert(t.pid, PgsTrackInfo {
                    track_id: t.pid as u32,
                    language: t.language.clone(),
                    container,
                    name: None,
                    flag_default: None,
                    flag_forced: None,
                    display_set_count: None,
                });
            }
        }

        // Build PES + display set assemblers for active PIDs.
        let mut pid_state = HashMap::new();
        for &pid in &active_pids {
            pid_state.insert(pid, (PesReassembler::new(), DisplaySetAssembler::new()));
        }

        Self {
            reader,
            format: metadata.format,
            pid_state,
            track_info,
            scan_end: metadata.file_size,
            pending: VecDeque::new(),
            flushed: false,
            container,
        }
    }

    /// Advance the state machine to produce the next display set.
    pub(crate) fn next_display_set(&mut self) -> Option<Result<TrackDisplaySet, PgsError>> {
        loop {
            // Drain pending display sets first.
            if let Some(tds) = self.pending.pop_front() {
                return Some(Ok(tds));
            }

            // If we've finished scanning and flushing, we're done.
            if self.flushed {
                return None;
            }

            // If we've reached end of scan, flush PES reassemblers.
            if self.reader.position() >= self.scan_end {
                self.flush_all();
                self.flushed = true;
                // Check if flush produced any display sets.
                if !self.pending.is_empty() {
                    continue;
                }
                return None;
            }

            // Process the next 2 MB block of packets.
            match self.scan_next_block() {
                Ok(()) => continue,
                Err(e) => {
                    self.flushed = true;
                    return Some(Err(e));
                }
            }
        }
    }

    /// Scan one block of TS packets (up to SCAN_BLOCK_SIZE bytes).
    fn scan_next_block(&mut self) -> Result<(), PgsError> {
        let packet_size = self.format.packet_size();
        let sync_offset = self.format.sync_offset();
        let end = self.scan_end;

        if self.reader.position() >= end {
            return Ok(());
        }

        let remaining = (end - self.reader.position()) as usize;
        let read_size = remaining.min(SCAN_BLOCK_SIZE);

        if read_size < packet_size {
            // Not enough data for a single packet — we're done scanning.
            self.reader.seek_to(end)?;
            return Ok(());
        }

        let block_start = self.reader.position();
        let block = self.reader.read_bytes(read_size)?;

        // Find packet alignment.
        let mut offset = 0;
        if offset + sync_offset < block.len() && block[offset + sync_offset] != ts_packet::SYNC_BYTE {
            match find_sync_start(&block, sync_offset, packet_size) {
                Some(pos) => offset = pos,
                None => {
                    let scan_limit = (end - self.reader.position()).min(MAX_RESYNC_SCAN);
                    if ts_packet::resync(&mut self.reader, self.format, scan_limit)?.is_none() {
                        self.reader.seek_to(end)?;
                    }
                    return Ok(());
                }
            }
        }

        // Bulk scan packets.
        while offset + packet_size <= block.len() {
            let ts_pos = offset + sync_offset;

            if block[ts_pos] != ts_packet::SYNC_BYTE {
                match find_sync_start(&block[offset + 1..], sync_offset, packet_size) {
                    Some(resync_pos) => {
                        offset = offset + 1 + resync_pos;
                        continue;
                    }
                    None => {
                        let loss_pos = block_start + offset as u64 + 1;
                        self.reader.seek_to(loss_pos)?;
                        let scan_limit = (end - self.reader.position()).min(MAX_RESYNC_SCAN);
                        if ts_packet::resync(&mut self.reader, self.format, scan_limit)?.is_none() {
                            self.reader.seek_to(end)?;
                        }
                        return Ok(());
                    }
                }
            }

            // Quick PID check.
            let pid = ((block[ts_pos + 1] as u16 & 0x1F) << 8) | block[ts_pos + 2] as u16;

            if let Some((pes_asm, ds_asm)) = self.pid_state.get_mut(&pid) {
                let ts_data: &[u8; ts_packet::TS_PACKET_SIZE] =
                    block[ts_pos..ts_pos + ts_packet::TS_PACKET_SIZE]
                        .try_into()
                        .unwrap();

                if let Ok((header, payload)) = ts_packet::extract_payload(ts_data) {
                    if header.has_payload() && !payload.is_empty() {
                        let segments = pes_asm.push(header.pusi, payload);
                        for seg in segments {
                            if let Some(ds) = ds_asm.push(seg) {
                                if let Some(info) = self.track_info.get(&pid) {
                                    self.pending.push_back(TrackDisplaySet {
                                        track_id: info.track_id,
                                        language: info.language.clone(),
                                        container: self.container,
                                        display_set: ds,
                                    });
                                }
                            }
                        }
                    }
                }
            }

            offset += packet_size;
        }

        // Rewind for incomplete packet at end of block.
        if offset < block.len() {
            let unprocessed = (block.len() - offset) as u64;
            self.reader.seek_to(self.reader.position() - unprocessed)?;
        }

        Ok(())
    }

    /// Flush all PES reassemblers to emit any final segments.
    fn flush_all(&mut self) {
        let pids: Vec<u16> = self.pid_state.keys().copied().collect();
        for pid in pids {
            if let Some((pes_asm, ds_asm)) = self.pid_state.get_mut(&pid) {
                for seg in pes_asm.flush() {
                    if let Some(ds) = ds_asm.push(seg) {
                        if let Some(info) = self.track_info.get(&pid) {
                            self.pending.push_back(TrackDisplaySet {
                                track_id: info.track_id,
                                language: info.language.clone(),
                                container: self.container,
                                display_set: ds,
                            });
                        }
                    }
                }
            }
        }
    }

    /// Get current bytes read from the underlying reader.
    pub(crate) fn bytes_read(&self) -> u64 {
        self.reader.bytes_read()
    }
}
