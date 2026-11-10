//! Bounded runtime diagnostics, independent of Flutter's live log buffer.
//!
//! Only the app-directory call and shutdown touch files or wait for the worker.
//! Producers enqueue capped records; neither a full queue nor slow storage can
//! backpressure a tunnel. The standard tracing worker drains batches and flushes.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rolling_file::{BasicRollingFileAppender, RollingConditionBasic};
use tracing_appender::non_blocking::{ErrorCounter, NonBlocking, NonBlockingBuilder, WorkerGuard};

const QUEUE_RECORDS: usize = 512;
const RECORD_BYTES: usize = 16 * 1024;
const FILE_BYTES: u64 = 8 * 1024 * 1024;
const ARCHIVES: usize = 3;
const RETENTION_SECONDS: u64 = 60 * 60;
const SLOT_BYTES: u64 = FILE_BYTES * (ARCHIVES as u64 + 1);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(15);
// A nonempty internal command: std::io::Write::write_all skips empty buffers.
// A formatted tracing record always starts with a timestamp, never this byte.
const MAINTENANCE: &[u8] = b"\0";
// Separate fixed slots let simultaneous UI processes log without renaming one
// another's open files, while keeping retention bounded across process restarts.
const WRITER_SLOTS: usize = 4;
const TRUNCATED: &[u8] = b" ... [record truncated]\n";
const _: () = assert!(
    TRUNCATED.len() < RECORD_BYTES,
    "a truncated record keeps some of its text"
);
const _: () = assert!(
    RECORD_BYTES as u64 <= FILE_BYTES,
    "one record fits in one file"
);
const _: () = assert!(
    RECORD_BYTES as u64 <= SLOT_BYTES,
    "SLOT_BYTES less a record cannot underflow"
);

static LOG: LazyLock<DiskLog> = LazyLock::new(DiskLog::default);

#[derive(Default)]
struct DiskLog {
    sink: Mutex<Sink>,
    // Serialize configure/shutdown separately: the producer never takes this
    // lock, even while a directory is being opened or an old worker is drained.
    runtime: Mutex<Option<LogRuntime>>,
}

enum Sink {
    Pending {
        records: VecDeque<Vec<u8>>,
        dropped: usize,
    },
    Active(NonBlocking),
    Stopped,
}

impl Default for Sink {
    fn default() -> Self {
        Self::Pending {
            records: VecDeque::new(),
            dropped: 0,
        }
    }
}

struct LogRuntime {
    directory: PathBuf,
    guard: Option<WorkerGuard>,
    maintenance_stop: mpsc::Sender<()>,
}

impl Drop for LogRuntime {
    fn drop(&mut self) {
        let _ = self.maintenance_stop.send(());
        self.guard.take();
    }
}

impl DiskLog {
    fn write(&self, bytes: &[u8]) {
        // tracing's formatter writes UTF-8. Bound both the bootstrap buffer and
        // each message copied into tracing-appender's line-bounded channel.
        let record = if bytes.len() > RECORD_BYTES {
            let mut end = RECORD_BYTES - TRUNCATED.len();
            while end > 0 && bytes[end] & 0xc0 == 0x80 {
                end -= 1;
            }
            [bytes[..end].as_ref(), TRUNCATED].concat()
        } else {
            bytes.to_vec()
        };
        match &mut *self.sink.lock() {
            Sink::Pending { records, dropped } => {
                if records.len() == QUEUE_RECORDS {
                    records.pop_front();
                    *dropped = dropped.saturating_add(1);
                }
                records.push_back(record);
            }
            Sink::Active(writer) => {
                let _ = writer.write_all(&record);
            }
            Sink::Stopped => {}
        }
    }

    fn configure(&self, directory: &Path) -> io::Result<()> {
        let mut runtime = self.runtime.lock();
        if runtime
            .as_ref()
            .is_some_and(|current| current.directory == directory)
        {
            return Ok(());
        }
        let file = LogFile::open(directory, FILE_BYTES)?;
        let counter = file.counter.clone();
        let (mut writer, guard) = non_blocking(file);
        let _ = counter.set(writer.error_counter());
        let (maintenance_stop, stop) = mpsc::channel();
        let mut maintenance_writer = writer.clone();
        let maintenance_directory = directory.to_path_buf();
        std::thread::Builder::new()
            .name("pb-mapper-log-retention".into())
            .spawn(move || {
                while stop.recv_timeout(CLEANUP_INTERVAL) == Err(mpsc::RecvTimeoutError::Timeout) {
                    // The worker command advances retention even with no
                    // application traffic; it writes no heartbeat log line.
                    let _ = maintenance_writer.write_all(MAINTENANCE);
                    if let Err(error) =
                        cleanup_inactive_slots(&maintenance_directory, unix_seconds())
                    {
                        report_error(&format!("Runtime log cleanup failed: {error}"));
                    }
                }
            })?;
        // Replaying is also nonblocking. The worker is already draining the
        // queue; excess records remain observable through its dropped counter.
        {
            let mut sink = self.sink.lock();
            if let Sink::Pending { records, dropped } = &mut *sink {
                if *dropped > 0 {
                    let _ = writeln!(
                        writer,
                        "WARN bootstrap log buffer overflow: {dropped} records lost"
                    );
                }
                for record in records.drain(..) {
                    let _ = writer.write_all(&record);
                }
            }
            *sink = Sink::Active(writer);
        }
        // The old guard drains outside the producer lock. No callbacks or
        // filesystem operations run while that lock is held.
        *runtime = Some(LogRuntime {
            directory: directory.to_path_buf(),
            guard: Some(guard),
            maintenance_stop,
        });
        Ok(())
    }

    fn shutdown(&self) {
        let mut runtime = self.runtime.lock();
        *self.sink.lock() = Sink::Stopped;
        // WorkerGuard waits at most 100 ms to enqueue shutdown, then 1 s for
        // draining. A wedged disk must not prevent the application from exiting.
        runtime.take();
    }
}

/// A lock is held by the worker for the lifetime of its rolling files. Locking
/// a sidecar keeps the actual diagnostics readable on Windows.
struct LogFile {
    writer: Option<BasicRollingFileAppender>,
    _lease: File,
    directory: PathBuf,
    bucket: u64,
    bytes: u64,
    total_bytes: u64,
    limit: u64,
    counter: Arc<OnceLock<ErrorCounter>>,
    reported_drops: usize,
}

impl LogFile {
    fn open(directory: &Path, limit: u64) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        cleanup_inactive_slots(directory, unix_seconds())?;
        for slot in 0..WRITER_SLOTS {
            let Some(lease) = lock_slot(directory, slot)? else {
                continue;
            };
            let directory = directory.join(format!("runtime-{slot}"));
            fs::create_dir_all(&directory)?;
            let mut file = Self {
                writer: None,
                _lease: lease,
                directory,
                bucket: 0,
                bytes: 0,
                total_bytes: 0,
                limit,
                counter: Arc::new(OnceLock::new()),
                reported_drops: 0,
            };
            file.maintain(unix_seconds())?;
            return Ok(file);
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "all four runtime log slots are in use",
        ))
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.append_at(bytes, unix_seconds())
    }

    fn maintain(&mut self, now: u64) -> io::Result<()> {
        if let Some(writer) = &mut self.writer {
            writer.flush()?;
        }
        if self.bucket != now / 60 {
            self.writer = None;
            self.bucket = now / 60;
            self.bytes = 0;
        }
        self.total_bytes = cleanup(
            &self.directory,
            now,
            SLOT_BYTES,
            self.writer.as_ref().map(|_| self.bucket),
        )?;
        Ok(())
    }

    fn append_at(&mut self, bytes: &[u8], now: u64) -> io::Result<()> {
        if bytes.is_empty() || self.bucket != now / 60 {
            self.maintain(now)?;
        }
        if bytes.is_empty() {
            return Ok(());
        }
        if self.writer.is_none() {
            let path = self.directory.join(format!("runtime-{}.log", self.bucket));
            self.writer = Some(BasicRollingFileAppender::new(
                &path,
                RollingConditionBasic::new(),
                ARCHIVES,
            )?);
            self.bytes = fs::metadata(path)?.len();
        }
        // Explicit rollover propagates errors. The library's automatic size
        // condition instead continues growing the old file when rename fails.
        if self.bytes.saturating_add(bytes.len() as u64) > self.limit {
            if let Some(writer) = &mut self.writer {
                writer.rollover()?;
            }
            self.bytes = 0;
            self.total_bytes = cleanup(&self.directory, now, SLOT_BYTES, Some(self.bucket))?;
        }
        if self.total_bytes.saturating_add(bytes.len() as u64) > SLOT_BYTES {
            if let Some(writer) = &mut self.writer {
                writer.flush()?;
            }
            self.total_bytes = cleanup(
                &self.directory,
                now,
                SLOT_BYTES - bytes.len() as u64,
                Some(self.bucket),
            )?;
        }
        if let Some(writer) = &mut self.writer {
            writer.write_all(bytes)?;
        }
        self.bytes += bytes.len() as u64;
        self.total_bytes += bytes.len() as u64;
        Ok(())
    }
}

impl Write for LogFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = if bytes == MAINTENANCE {
            self.maintain(unix_seconds())
        } else {
            self.append(bytes)
        };
        result.map(|()| bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let dropped = self.counter.get().map_or(0, ErrorCounter::dropped_lines);
        if dropped > self.reported_drops {
            self.append(
                format!(
                    "WARN runtime log queue overflow: {} records lost\n",
                    dropped - self.reported_drops
                )
                .as_bytes(),
            )?;
            self.reported_drops = dropped;
        }
        self.writer.as_mut().map_or(Ok(()), Write::flush)
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn non_blocking(writer: impl Write + Send + 'static) -> (NonBlocking, WorkerGuard) {
    NonBlockingBuilder::default()
        .buffered_lines_limit(QUEUE_RECORDS)
        .lossy(true)
        .thread_name("pb-mapper-log")
        .finish(ReportingWriter {
            inner: writer,
            failed: false,
            pending_write: false,
        })
}

/// The asynchronous worker cannot retry a failed record. Consume it after
/// reporting the failure so an I/O error cannot prevent the worker from handling
/// shutdown. In particular, tracing-appender otherwise loses its shutdown state
/// when flush fails and can repeatedly flush a disconnected channel.
struct ReportingWriter<W> {
    inner: W,
    failed: bool,
    pending_write: bool,
}

impl<W: Write> ReportingWriter<W> {
    fn report_failure(&mut self, error: io::Error) {
        self.pending_write = false;
        if !self.failed {
            self.failed = true;
            report_error(&format!("Runtime log persistence failed: {error}"));
        }
    }
}

impl<W: Write> Write for ReportingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.inner.write_all(bytes) {
            Err(error) => self.report_failure(error),
            Ok(()) if bytes != MAINTENANCE => self.pending_write = true,
            Ok(()) => {}
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.inner.flush() {
            Err(error) => self.report_failure(error),
            Ok(()) => {
                if self.failed && self.pending_write {
                    self.failed = false;
                    report_error("Runtime log persistence recovered; records may have been lost");
                }
                self.pending_write = false;
            }
        }
        Ok(())
    }
}

fn lock_slot(directory: &Path, slot: usize) -> io::Result<Option<File>> {
    let lease = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(format!(".runtime-{slot}.lock")))?;
    match lease.try_lock() {
        Ok(()) => Ok(Some(lease)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

fn cleanup_inactive_slots(directory: &Path, now: u64) -> io::Result<()> {
    for slot in 0..WRITER_SLOTS {
        if let Some(_lease) = lock_slot(directory, slot)? {
            let path = directory.join(format!("runtime-{slot}"));
            if path.is_dir() {
                cleanup(&path, now, SLOT_BYTES, None)?;
            }
        }
    }
    Ok(())
}

/// Whole minute shards crossing the one-hour boundary are removed. This may
/// discard up to 59 seconds of younger records in the oldest shard. Maintenance
/// runs every 15 seconds, including while idle, and again on each minute change.
fn cleanup(directory: &Path, now: u64, budget: u64, active: Option<u64>) -> io::Result<u64> {
    let mut files = Vec::new();
    let cutoff = now.saturating_sub(RETENTION_SECONDS);
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().and_then(|name| name.strip_prefix("runtime-")) else {
            continue;
        };
        let Some((minute, suffix)) = name.split_once(".log") else {
            continue;
        };
        let Ok(minute) = minute.parse::<u64>() else {
            continue;
        };
        let archive = match suffix {
            "" => 0,
            _ => match suffix
                .strip_prefix('.')
                .and_then(|s| s.parse::<usize>().ok())
            {
                Some(number @ 1..=ARCHIVES) => number,
                _ => continue,
            },
        };
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if minute.saturating_mul(60) < cutoff {
            fs::remove_file(path)?;
        } else {
            files.push((minute, archive, entry.metadata()?.len(), path));
        }
    }
    files.sort_by_key(|(minute, archive, _, _)| (*minute, std::cmp::Reverse(*archive)));
    let mut total: u64 = files.iter().map(|(_, _, size, _)| size).sum();
    for (minute, archive, size, path) in files {
        if total <= budget {
            break;
        }
        if active == Some(minute) && archive == 0 {
            continue;
        }
        fs::remove_file(path)?;
        total -= size;
    }
    Ok(total)
}

fn report_error(message: &str) {
    // Never feed worker errors back into the same logging queue.
    eprintln!("{message}");
    crate::logging::send_log(4, message);
}

#[derive(Clone, Copy)]
pub(crate) struct DiskWriter;

impl Write for DiskWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        LOG.write(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn configure(config_directory: &Path) {
    let directory = config_directory.join("logs");
    match LOG.configure(&directory) {
        Ok(()) => tracing::info!(directory = %directory.display(), "Runtime logs enabled"),
        Err(error) => report_error(&format!(
            "Cannot persist runtime logs in {}: {error}",
            directory.display()
        )),
    }
}

pub(crate) fn shutdown() {
    LOG.shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::TempRoot;

    /// Every record written into `directory`'s slots.
    fn slot_records(directory: &Path) -> String {
        (0..WRITER_SLOTS)
            .filter_map(|slot| fs::read_dir(directory.join(format!("runtime-{slot}"))).ok())
            .flatten()
            .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect()
    }

    #[test]
    fn bootstrap_is_bounded_and_shutdown_persists_without_a_ui_callback() {
        let directory = TempRoot::new("runtime-log");
        let logger = DiskLog::default();
        let huge = "中文".repeat(RECORD_BYTES);
        for _ in 0..QUEUE_RECORDS + 2 {
            logger.write(huge.as_bytes());
        }
        {
            let sink = logger.sink.lock();
            let Sink::Pending { records, dropped } = &*sink else {
                panic!("missing bootstrap buffer");
            };
            assert_eq!(*dropped, 2);
            assert_eq!(records.len(), QUEUE_RECORDS);
            for record in records {
                assert!(record.len() <= RECORD_BYTES);
                assert!(
                    std::str::from_utf8(record)
                        .unwrap()
                        .ends_with("[record truncated]\n")
                );
            }
        }
        // Use a fresh logger to test complete normal startup + shutdown, without
        // intentionally overflowing the asynchronous channel at the same time.
        let logger = DiskLog::default();
        logger.write(b"startup before the app directory\n");
        logger.configure(directory.path()).unwrap();
        logger.configure(directory.path()).unwrap();
        for index in 0..100 {
            logger.write(format!("native event {index} 中文\n").as_bytes());
        }
        logger.shutdown();
        logger.shutdown();
        let records = slot_records(directory.path());
        assert!(records.contains("startup before the app directory"));
        assert_eq!(records.matches("native event").count(), 100);
        // Reinitialization appends and a failed reconfiguration preserves the
        // active sink instead of silently replacing it with no persistence.
        logger.configure(directory.path()).unwrap();
        let invalid = directory.path().join("not-a-directory");
        fs::write(&invalid, "file").unwrap();
        assert!(logger.configure(&invalid).is_err());
        logger.write(b"after restart\n");
        logger.shutdown();
        assert!(slot_records(directory.path()).contains("after restart"));
    }

    #[test]
    fn retention_expires_idle_shards_and_rotates_with_finite_capacity() {
        let directory = TempRoot::new("runtime-log");
        let mut file = LogFile::open(directory.path(), 80).unwrap();
        let now = unix_seconds() / 60 * 60;
        for index in 0..30 {
            file.append_at(
                format!("record {index:02} 中文0123456789\n").as_bytes(),
                now,
            )
            .unwrap();
        }
        file.flush().unwrap();
        let files: Vec<_> = fs::read_dir(&file.directory)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(files.len(), ARCHIVES + 1);
        assert!(
            files
                .iter()
                .all(|entry| entry.metadata().unwrap().len() <= 80)
        );
        let kept = slot_records(directory.path());
        assert!(kept.contains("record 29"));
        assert!(!kept.contains("record 00"));
        // A different live writer claims a different slot.
        let other = LogFile::open(directory.path(), 80).unwrap();
        assert_ne!(file.directory, other.directory);
        // An idle maintenance tick closes the old file and removes it by time,
        // even when no replacement record (and no new file) is created.
        file.maintain(now + RETENTION_SECONDS + 1).unwrap();
        assert_eq!(fs::read_dir(&file.directory).unwrap().count(), 0);
        let old = file.directory.join(format!("runtime-{}.log", now / 60));
        let recent = file
            .directory
            .join(format!("runtime-{}.log", now / 60 + 60));
        fs::write(&old, b"expired").unwrap();
        fs::write(&recent, b"recent").unwrap();
        let slot_directory = file.directory.clone();
        drop(file);
        cleanup_inactive_slots(directory.path(), now + RETENTION_SECONDS + 1).unwrap();
        assert!(!old.exists());
        assert!(recent.exists());
        // Capacity pressure can discard younger archives, while unrelated files
        // are outside the retention policy and are never deleted.
        fs::write(slot_directory.join("notes.txt"), "keep").unwrap();
        fs::write(format!("{}.1", recent.display()), b"archive").unwrap();
        assert_eq!(
            cleanup(&slot_directory, now + RETENTION_SECONDS + 1, 6, None).unwrap(),
            6
        );
        assert!(slot_directory.join("notes.txt").exists());

        // Exercise the real nonblocking worker command too: an empty write
        // would be swallowed by std::io::Write::write_all before reaching us.
        let idle_directory = TempRoot::new("runtime-log");
        let mut idle = LogFile::open(idle_directory.path(), 80).unwrap();
        idle.append_at(b"expired idle record\n", now - RETENTION_SECONDS - 60)
            .unwrap();
        idle.flush().unwrap();
        let idle_path = idle.directory.clone();
        let (mut writer, guard) = non_blocking(idle);
        writer.write_all(MAINTENANCE).unwrap();
        drop(guard);
        assert_eq!(fs::read_dir(idle_path).unwrap().count(), 0);
    }

    #[test]
    fn a_stalled_disk_drops_excess_records_without_blocking_producers() {
        struct Gate {
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
            blocked: bool,
        }
        impl Write for Gate {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if !self.blocked {
                    self.blocked = true;
                    self.entered.send(()).unwrap();
                    self.release.recv().unwrap();
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (entered, receive_entered) = mpsc::channel();
        let (release, receive_release) = mpsc::channel();
        let (mut writer, guard) = non_blocking(Gate {
            entered,
            release: receive_release,
            blocked: false,
        });
        writer.write_all(b"first").unwrap();
        receive_entered
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let counter = writer.error_counter();
        let (finished, receive_finished) = mpsc::channel();
        let producer = std::thread::spawn(move || {
            for _ in 0..QUEUE_RECORDS + 7 {
                writer.write_all(b"queued").unwrap();
            }
            finished.send(()).unwrap();
        });
        let result = receive_finished.recv_timeout(Duration::from_secs(2));
        release.send(()).unwrap();
        producer.join().unwrap();
        result.unwrap();
        assert_eq!(counter.dropped_lines(), 7);
        drop(guard);
    }

    #[test]
    fn a_failed_disk_still_shuts_down_and_releases_its_writer() {
        struct FailedDisk(mpsc::Sender<()>);
        impl Write for FailedDisk {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("storage unavailable"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("storage unavailable"))
            }
        }
        impl Drop for FailedDisk {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }

        let (released, receive_released) = mpsc::channel();
        let (mut writer, guard) = non_blocking(FailedDisk(released));
        writer
            .write_all(b"record lost to storage failure\n")
            .unwrap();
        drop(writer);
        drop(guard);
        receive_released
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
    }
}
