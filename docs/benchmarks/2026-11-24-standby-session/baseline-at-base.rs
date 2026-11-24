
/// The standby baseline at the base commit, self-contained: a relayed
/// session whose link holds every byte from 6 s after connecting, ending at
/// the read timeout, then a timed reconnect over another link.
#[test]
#[ignore = "explicit baseline at the base commit"]
fn standby_baseline_at_base() {
    use pb_mapper_testkit::shaped_link;
    struct H264;
    impl Backend for H264 {
        fn display(&mut self) -> Result<Display> {
            Ok(Display {
                id: 1,
                name: "standby".into(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            })
        }
        fn capture(&mut self) -> Result<Vec<u8>> {
            Ok(vec![1])
        }
        fn capture_video(
            &mut self,
            _: &DesktopContext,
            codec: VideoCodec,
            keyframe: bool,
            _: VideoRate,
        ) -> Result<Option<EncodedFrame>> {
            Ok(Some(EncodedFrame {
                format: VideoFormat {
                    codec,
                    width: 1920,
                    height: 1080,
                    keyframe,
                    ..VideoFormat::default()
                },
                bytes: vec![if keyframe { 0x65 } else { 0x41 }; 64],
            }))
        }
        fn input(&mut self, _: &Input) -> Result<()> {
            Ok(())
        }
        fn release(&mut self) {}
    }
    fn takes_input(viewer: &Viewer) -> bool {
        let status = viewer.status();
        status.controlling
            && status.fresh
            && viewer
                .input(status.generation, &[Input::Move { x: 1, y: 1 }])
                .is_ok()
    }
    async fn first<T>(mut probe: impl FnMut() -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Some(value) = probe() {
                return Some(value);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        None
    }
    fn spread(values: &mut [f64]) -> (f64, f64, f64) {
        values.sort_by(f64::total_cmp);
        let at = |fraction: f64| {
            let rank = ((values.len() as f64 * fraction).ceil() as usize).clamp(1, values.len());
            values[rank - 1]
        };
        (at(0.5), at(0.9), at(1.0))
    }
    const CASE: &str = "standby_baseline_base";
    traced(|_| async move {
        let (mut ends, mut pictures, mut inputs) = (Vec::new(), Vec::new(), Vec::new());
        for run in 0..10 {
            let relay = Relay::start(&format!("desktop-base-standby-{run}")).await;
            let client = |addr: SocketAddr| {
                Client::from_credential(addr.to_string(), admin_credential(), true, None)
            };
            let mut options = HostOptions::new(format!("desktop:base-standby-{run}"));
            options.allow_control = true;
            options.allow_direct = false;
            let host = Host::builder(options)
                .backend(|| Ok(H264))
                .start(&client(relay.addr()))
                .await
                .unwrap();
            let live = Arc::new(Shape::default());
            let links = [
                shaped_link(relay.addr(), live.clone()).await,
                shaped_link(relay.addr(), Arc::new(Shape::default())).await,
            ];
            let options = || ConnectOptions {
                path: PathPolicy::RelayOnly,
                ..Default::default()
            };
            let connecting = Instant::now();
            let viewer = Viewer::connect(
                &client(links[0].addr()),
                host.invitation(),
                options(),
                ViewerConfig::new(vec![VideoCodec::H264]),
            )
            .await
            .unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let renderer = crate::test_util::present(&viewer, stop.clone());
            first(|| viewer.status().fresh.then_some(())).await.unwrap();
            viewer.request_control(true).unwrap();
            first(|| takes_input(&viewer).then_some(())).await.unwrap();
            tokio::time::sleep_until((connecting + Duration::from_secs(6)).into()).await;
            let blocked = Instant::now();
            live.blocked.store(true, Ordering::Relaxed);
            let ended = first(|| (viewer.status().state == ViewerState::Stopped).then(Instant::now))
                .await
                .unwrap();
            stop.store(true, Ordering::Release);
            renderer.join().unwrap();
            let reconnecting = Instant::now();
            let again = Viewer::connect(
                &client(links[1].addr()),
                host.invitation(),
                options(),
                ViewerConfig::new(vec![VideoCodec::H264]),
            )
            .await
            .unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let renderer = crate::test_util::present(&again, stop.clone());
            let picture = first(|| again.status().fresh.then(Instant::now))
                .await
                .unwrap();
            again.request_control(true).unwrap();
            let input = first(|| takes_input(&again).then(Instant::now)).await.unwrap();
            let end_ms = ended.duration_since(blocked).as_secs_f64() * 1000.0;
            let picture_ms = picture.duration_since(reconnecting).as_secs_f64() * 1000.0;
            let input_ms = input.duration_since(reconnecting).as_secs_f64() * 1000.0;
            println!(
                "{CASE} run={run} ended_after_block_ms={end_ms:.0} reconnect_picture_ms={picture_ms:.0} reconnect_input_ms={input_ms:.0} reason={:?}",
                viewer.status().reason,
            );
            ends.push(end_ms);
            pictures.push(picture_ms);
            inputs.push(input_ms);
            again.stop().await.unwrap();
            stop.store(true, Ordering::Release);
            renderer.join().unwrap();
            host.stop().await.unwrap();
        }
        let (end_p50, end_p90, end_max) = spread(&mut ends);
        let (picture_p50, picture_p90, picture_max) = spread(&mut pictures);
        let (input_p50, input_p90, input_max) = spread(&mut inputs);
        println!(
            "{CASE} summary runs=10 ended_after_block_ms p50={end_p50:.0} p90={end_p90:.0} max={end_max:.0} reconnect_picture_ms p50={picture_p50:.0} p90={picture_p90:.0} max={picture_max:.0} reconnect_input_ms p50={input_p50:.0} p90={input_p90:.0} max={input_max:.0}",
        );
    });
}
