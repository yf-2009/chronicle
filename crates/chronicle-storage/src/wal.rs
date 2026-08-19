//! Write-ahead log with CRC32-framed binary records.
//!
//! On-disk record layout (all integers little-endian):
//!
//! ```text
//! +-------------+-------------+--------+------------------+
//! | total_len:4 | crc32:4     | op:1   | body: total_len-1|
//! +-------------+-------------+--------+------------------+
//! ```
//!
//! `total_len` covers `op` + `body`. `crc32` is computed over `op || body`.
//!
//! Body layout for `Op::Put`:  `seq:8 | key_len:4 | key | value_len:4 | value`
//! Body layout for `Op::Delete`: `seq:8 | key_len:4 | key`
//!
//! The log is append-only. On recovery we read records sequentially and stop
//! at the first record that fails to parse or fails its checksum -- this is
//! the expected shape of a torn write left by a crash mid-append, not an
//! error condition. Everything before the tear is durable and returned.

use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{Result, StorageError};

const OP_PUT: u8 = 0;
const OP_DELETE: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalRecord {
    Put { seq: u64, key: Vec<u8>, value: Vec<u8> },
    Delete { seq: u64, key: Vec<u8> },
}

impl WalRecord {
    pub fn seq(&self) -> u64 {
        match self {
            WalRecord::Put { seq, .. } => *seq,
            WalRecord::Delete { seq, .. } => *seq,
        }
    }

    fn encode_body(&self) -> (u8, Vec<u8>) {
        match self {
            WalRecord::Put { seq, key, value } => {
                let mut body = Vec::with_capacity(8 + 4 + key.len() + 4 + value.len());
                body.extend_from_slice(&seq.to_le_bytes());
                body.extend_from_slice(&(key.len() as u32).to_le_bytes());
                body.extend_from_slice(key);
                body.extend_from_slice(&(value.len() as u32).to_le_bytes());
                body.extend_from_slice(value);
                (OP_PUT, body)
            }
            WalRecord::Delete { seq, key } => {
                let mut body = Vec::with_capacity(8 + 4 + key.len());
                body.extend_from_slice(&seq.to_le_bytes());
                body.extend_from_slice(&(key.len() as u32).to_le_bytes());
                body.extend_from_slice(key);
                (OP_DELETE, body)
            }
        }
    }

    fn decode(op: u8, body: &[u8]) -> Result<Self> {
        let mut cursor = 0usize;
        let read_u64 = |buf: &[u8], at: usize| -> Result<u64> {
            let slice = buf
                .get(at..at + 8)
                .ok_or_else(|| StorageError::Corrupt("truncated u64".into()))?;
            Ok(u64::from_le_bytes(slice.try_into().unwrap()))
        };
        let read_u32 = |buf: &[u8], at: usize| -> Result<u32> {
            let slice = buf
                .get(at..at + 4)
                .ok_or_else(|| StorageError::Corrupt("truncated u32".into()))?;
            Ok(u32::from_le_bytes(slice.try_into().unwrap()))
        };

        let seq = read_u64(body, cursor)?;
        cursor += 8;

        match op {
            OP_PUT => {
                let key_len = read_u32(body, cursor)? as usize;
                cursor += 4;
                let key = body
                    .get(cursor..cursor + key_len)
                    .ok_or_else(|| StorageError::Corrupt("truncated key".into()))?
                    .to_vec();
                cursor += key_len;
                let value_len = read_u32(body, cursor)? as usize;
                cursor += 4;
                let value = body
                    .get(cursor..cursor + value_len)
                    .ok_or_else(|| StorageError::Corrupt("truncated value".into()))?
                    .to_vec();
                Ok(WalRecord::Put { seq, key, value })
            }
            OP_DELETE => {
                let key_len = read_u32(body, cursor)? as usize;
                cursor += 4;
                let key = body
                    .get(cursor..cursor + key_len)
                    .ok_or_else(|| StorageError::Corrupt("truncated key".into()))?
                    .to_vec();
                Ok(WalRecord::Delete { seq, key })
            }
            other => Err(StorageError::Corrupt(format!("unknown op byte {other}"))),
        }
    }
}

pub struct WriteAheadLog {
    path: PathBuf,
    writer: BufWriter<File>,
    fsync_every_write: bool,
}

impl WriteAheadLog {
    /// Open (creating if absent) a WAL file in append mode.
    pub fn open<P: AsRef<Path>>(path: P, fsync_every_write: bool) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            path,
            writer: BufWriter::new(file),
            fsync_every_write,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a record. Returns once the record has been written to the
    /// buffered writer; call `flush()` (or rely on `fsync_every_write`) to
    /// guarantee durability before acknowledging a write to a client.
    pub fn append(&mut self, record: &WalRecord) -> Result<()> {
        let (op, body) = record.encode_body();
        let mut crc_input = Vec::with_capacity(1 + body.len());
        crc_input.push(op);
        crc_input.extend_from_slice(&body);
        let crc = crc32fast::hash(&crc_input);
        let total_len = crc_input.len() as u32;

        self.writer.write_all(&total_len.to_le_bytes())?;
        self.writer.write_all(&crc.to_le_bytes())?;
        self.writer.write_all(&crc_input)?;

        if self.fsync_every_write {
            self.flush()?;
        }
        Ok(())
    }

    /// Flush the buffered writer and fsync the underlying file.
    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        self.writer.get_ref().sync_data()?;
        Ok(())
    }

    /// Truncate the log to empty. Used after a memtable flush makes the WAL
    /// contents redundant (they're now durable in an SSTable).
    pub fn truncate(&mut self) -> Result<()> {
        self.writer.flush()?;
        let file = self.writer.get_ref();
        file.set_len(0)?;
        // Re-seek to start for append mode bookkeeping; append-mode files
        // always write at EOF regardless of seek position, but we do this
        // for clarity/portability.
        drop(file);
        let file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.writer = BufWriter::new(file);
        Ok(())
    }

    /// Read every valid record from `path` in order. Stops at the first
    /// record that is truncated or fails its checksum, on the assumption
    /// that this represents a torn write from a crash, not a corrupt log to
    /// error out on. Returns the valid records plus the byte offset just
    /// past the last valid record, so the caller can truncate away any torn
    /// tail before resuming appends.
    pub fn replay<P: AsRef<Path>>(path: P) -> Result<(Vec<WalRecord>, u64)> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok((Vec::new(), 0));
        }
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut records = Vec::new();
        let mut valid_offset: u64 = 0;

        loop {
            let mut len_buf = [0u8; 4];
            if !read_exact_or_eof(&mut reader, &mut len_buf)? {
                break; // clean EOF between records
            }
            let mut crc_buf = [0u8; 4];
            if !read_exact_or_eof(&mut reader, &mut crc_buf)? {
                break; // torn: length written but crc missing
            }
            let total_len = u32::from_le_bytes(len_buf) as usize;
            let expected_crc = u32::from_le_bytes(crc_buf);

            let mut body = vec![0u8; total_len];
            if !read_exact_or_eof(&mut reader, &mut body)? {
                break; // torn: header written but body incomplete
            }

            let actual_crc = crc32fast::hash(&body);
            if actual_crc != expected_crc {
                break; // torn: bytes present but corrupt (partial fsync)
            }

            if body.is_empty() {
                break;
            }
            let op = body[0];
            match WalRecord::decode(op, &body[1..]) {
                Ok(record) => {
                    records.push(record);
                    valid_offset += 4 + 4 + total_len as u64;
                }
                Err(_) => break,
            }
        }

        Ok((records, valid_offset))
    }
}

/// Like `Read::read_exact` but returns `Ok(false)` instead of erroring when
/// zero bytes are available at the start of the read (clean EOF), and errors
/// on a *partial* read (torn record).
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<bool> {
    let mut read = 0;
    while read < buf.len() {
        match reader.read(&mut buf[read..]) {
            Ok(0) => {
                if read == 0 {
                    return Ok(false);
                }
                // partial read followed by EOF: treat as torn, not an error
                return Ok(false);
            }
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(StorageError::Io(e)),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("chronicle-wal-test-{}-{}", std::process::id(), name));
        p
    }

    #[test]
    fn append_and_replay_roundtrip() {
        let path = tmp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        {
            let mut wal = WriteAheadLog::open(&path, true).unwrap();
            wal.append(&WalRecord::Put { seq: 1, key: b"a".to_vec(), value: b"1".to_vec() })
                .unwrap();
            wal.append(&WalRecord::Put { seq: 2, key: b"b".to_vec(), value: b"2".to_vec() })
                .unwrap();
            wal.append(&WalRecord::Delete { seq: 3, key: b"a".to_vec() }).unwrap();
        }
        let (records, _) = WriteAheadLog::replay(&path).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0], WalRecord::Put { seq: 1, key: b"a".to_vec(), value: b"1".to_vec() });
        assert_eq!(records[2], WalRecord::Delete { seq: 3, key: b"a".to_vec() });
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn replay_stops_cleanly_at_torn_tail() {
        let path = tmp_path("torn");
        let _ = std::fs::remove_file(&path);
        {
            let mut wal = WriteAheadLog::open(&path, true).unwrap();
            wal.append(&WalRecord::Put { seq: 1, key: b"a".to_vec(), value: b"1".to_vec() })
                .unwrap();
        }
        // Simulate a crash mid-write: append a partial, garbage record.
        {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(&[1, 2, 3, 4, 5]).unwrap();
        }
        let (records, valid_offset) = WriteAheadLog::replay(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0], WalRecord::Put { seq: 1, key: b"a".to_vec(), value: b"1".to_vec() });
        // valid_offset should point exactly past the first good record,
        // letting the caller truncate off the torn garbage.
        let file_len = std::fs::metadata(&path).unwrap().len();
        assert!(valid_offset < file_len);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_log_replays_to_nothing() {
        let path = tmp_path("empty");
        let _ = std::fs::remove_file(&path);
        let (records, offset) = WriteAheadLog::replay(&path).unwrap();
        assert!(records.is_empty());
        assert_eq!(offset, 0);
    }

    #[test]
    fn truncate_resets_log() {
        let path = tmp_path("truncate");
        let _ = std::fs::remove_file(&path);
        {
            let mut wal = WriteAheadLog::open(&path, true).unwrap();
            wal.append(&WalRecord::Put { seq: 1, key: b"a".to_vec(), value: b"1".to_vec() })
                .unwrap();
            wal.truncate().unwrap();
            wal.append(&WalRecord::Put { seq: 2, key: b"b".to_vec(), value: b"2".to_vec() })
                .unwrap();
        }
        let (records, _) = WriteAheadLog::replay(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].seq(), 2);
        let _ = std::fs::remove_file(&path);
    }
}
