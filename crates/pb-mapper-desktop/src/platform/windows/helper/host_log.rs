//! The background host's log file, which the service writes.
use super::os;

/// The background host's log, beside `service.log`: its sessions, the
/// feedback that changes how it streams, and the path each viewer took, at
/// info. None of those targets logs an invitation, key or credential, and no
/// other target is written. One older copy is kept.
const HOST_LOG: &str = "service-host.log";
const HOST_LOG_BYTES: u64 = 1024 * 1024;

pub(super) fn trace_to_host_log() {
    use tracing_subscriber::{Layer, filter::Targets, layer::SubscriberExt};
    let targets = Targets::new()
        .with_target("pb_mapper_desktop::session", tracing::Level::INFO)
        .with_target("pb_mapper_desktop::feedback", tracing::Level::INFO)
        .with_target("pb_mapper_client::direct", tracing::Level::INFO);
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(HostLog::default())
        .with_filter(targets);
    let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(layer));
}

/// `HOST_LOG`, opened at the first record and rotated once it would pass
/// `HOST_LOG_BYTES`. A record that cannot be written is dropped.
#[derive(Default)]
struct HostLog(parking_lot::Mutex<Option<(std::fs::File, u64)>>);
struct HostLogRecord<'a>(parking_lot::MutexGuard<'a, Option<(std::fs::File, u64)>>);
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for HostLog {
    type Writer = HostLogRecord<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        HostLogRecord(self.0.lock())
    }
}
impl std::io::Write for HostLogRecord<'_> {
    fn write(&mut self, record: &[u8]) -> std::io::Result<usize> {
        let slot = &mut *self.0;
        let open = match slot {
            Some(open) => open,
            None => slot.insert(os::open_service_log(HOST_LOG)?),
        };
        if open.1 > 0 && open.1 + record.len() as u64 > HOST_LOG_BYTES {
            // Closed before it is renamed.
            *slot = None;
            os::rotate_service_log(HOST_LOG)?;
        }
        let (file, len) = match slot {
            Some(open) => open,
            None => slot.insert(os::open_service_log(HOST_LOG)?),
        };
        file.write_all(record)?;
        *len += record.len() as u64;
        Ok(record.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
