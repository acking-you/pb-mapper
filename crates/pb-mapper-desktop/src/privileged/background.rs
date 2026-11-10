//! The helper service's own host: configured by the app, owned by the
//! service, which keeps sharing while the app is closed.
pub use crate::platform::windows::helper::background::{
    Options, Settings, Snapshot, configure, configure_with_identity, end_session,
    restore_display_mode, revoke_scope, set_control_allowed, status, stop,
};
use crate::{Host, Result, StopReason, error};
use pb_mapper_client::sdk::RelayTransport;
use std::io;

/// Where a resumed background host publishes: the relay, credential and
/// carrier this computer uses now, with its LAN-first choice.
#[derive(Clone, Debug)]
pub struct ResumeTarget {
    pub server: String,
    /// The credential's namespace.
    pub namespace: u64,
    pub credential: String,
    pub relay_transport: RelayTransport,
    pub lan_first: bool,
}

impl Options {
    /// What resuming the `saved` background host configures: its service,
    /// display and permissions, restored at startup again, published through
    /// `target`. A host saved on another relay or namespace moves there only
    /// with `move_relay`, since moving changes its invitation; otherwise
    /// `ErrorKind::PermissionDenied`.
    pub fn resume(saved: &Snapshot, target: ResumeTarget, move_relay: bool) -> Result<Options> {
        let settings = saved
            .settings
            .clone()
            .filter(|_| saved.configured)
            .ok_or_else(|| error("no saved background host to resume"))?;
        let here = saved.server.as_deref() == Some(target.server.as_str())
            && saved.namespace == Some(target.namespace);
        if !move_relay && !here {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "this computer is shared on another relay or credential namespace; moving it changes its invitation",
            ));
        }
        Ok(Options {
            server: target.server,
            credential: target.credential,
            relay_transport: target.relay_transport,
            settings: Settings {
                auto_start: true,
                ..settings
            },
            lan_first: target.lan_first,
        })
    }
}

/// Hand a running local host over to the service under the same invitation.
/// It stops first, so that no two owners ever publish one desktop: its
/// viewer hears `Restarting` and reconnects when the service resumes this
/// invitation at once (`resumes_at_once`: the same identity and service, and
/// a helper that needs no setup), else `HostStopped`, rather than retrying a
/// host that may never come. Then the service is configured with the host's
/// identity.
pub async fn adopt(host: Host, options: Options, resumes_at_once: bool) -> Result<Snapshot> {
    let identity = host.identity().clone();
    let reason = if resumes_at_once {
        StopReason::Restarting
    } else {
        StopReason::HostStopped
    };
    host.stop_with(reason).await?;
    configure_with_identity(options, identity).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        HostOptions, VideoCodec, Viewer, ViewerConfig, ViewerReason, ViewerState, test_util::until,
    };
    use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy};

    struct Pixel;
    impl crate::Backend for Pixel {
        fn display(&mut self) -> Result<crate::Display> {
            Ok(crate::Display {
                id: 1,
                name: "adopted".into(),
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            })
        }
        fn capture(&mut self) -> Result<Vec<u8>> {
            Ok(vec![1])
        }
        fn input(&mut self, _: &crate::Input) -> Result<()> {
            Ok(())
        }
        fn release(&mut self) {}
    }

    /// The viewer of an adopted host reconnects by itself only when the
    /// service resumes its invitation at once; otherwise it ends, rather than
    /// retry a host that may never come. A test binary is never the helper's
    /// approved application, so configuring the service then fails, after the
    /// local host already stopped.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn adopting_tells_the_viewer_whether_its_invitation_resumes() {
        let relay = pb_mapper_testkit::Relay::start("desktop-adopt").await;
        let client = Client::from_credential(
            relay.addr().to_string(),
            pb_mapper_testkit::admin_credential(),
            true,
            None,
        );
        for (resumes_at_once, heard) in [
            (true, ViewerReason::Restarting),
            (false, ViewerReason::HostStopped),
        ] {
            let mut options = HostOptions::new("desktop:adopt");
            options.allow_direct = false;
            let host = Host::builder(options)
                .backend(|| Ok(Pixel))
                .start(&client)
                .await
                .unwrap();
            let viewer = Viewer::connect(
                &client,
                host.invitation(),
                ConnectOptions {
                    path: PathPolicy::RelayOnly,
                    ..Default::default()
                },
                ViewerConfig::new(vec![VideoCodec::Jpeg]),
            )
            .await
            .unwrap();
            // `Accepted` reaches the viewer before the host holds the session.
            until(|| host.status().viewer.is_some()).await;
            let service = Options {
                server: relay.addr().to_string(),
                credential: "background-test-key-000000000000".into(),
                relay_transport: RelayTransport::Tcp,
                settings: Settings {
                    service: "desktop:adopt".into(),
                    display_id: 1,
                    display_device: None,
                    allow_control: false,
                    allow_direct: true,
                    auto_start: true,
                },
                lan_first: true,
            };
            assert!(adopt(host, service, resumes_at_once).await.is_err());
            until(|| viewer.status().state == ViewerState::Stopped).await;
            assert_eq!(viewer.status().reason, Some(heard));
        }
    }
}
