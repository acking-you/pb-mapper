//! Release-mode cost of what a relay asks of the authentication state for
//! each new connection: deriving a key from its id, and authenticating the key
//! a peer presented, for a temporary key and, as a control, the administrator.
//! Run with `cargo run --release -p pb-mapper-auth --example auth_bench`.

use std::{error::Error, hint::black_box, time::Duration, time::Instant};

use pb_mapper_auth::{ADMIN_KEY_ID, AuthConfig, AuthRuntime, KeyId, LegacyProtocolPolicy};
use pb_mapper_core::test_support::{TEST_ADMIN_KEY_BYTES, TempPath};

const CALLS: usize = 200_000;
const SAMPLES: usize = 9;

/// Nanoseconds per call of `call`, the median of `SAMPLES` runs.
fn time(mut call: impl FnMut() -> Result<(), Box<dyn Error>>) -> Result<f64, Box<dyn Error>> {
    for _ in 0..CALLS / 10 {
        call()?;
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        for _ in 0..CALLS {
            call()?;
        }
        samples.push(start.elapsed().as_secs_f64() * 1e9 / CALLS as f64);
    }
    samples.sort_by(f64::total_cmp);
    Ok(samples[SAMPLES / 2])
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let state_dir = TempPath::new("auth-bench");
    let config = AuthConfig {
        state_dir: state_dir.to_path_buf(),
        max_temporary_keys: 16,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Deny,
    };
    let runtime = AuthRuntime::start(TEST_ADMIN_KEY_BYTES, config).await?;
    let admin = runtime.authenticate_presented(ADMIN_KEY_ID, &TEST_ADMIN_KEY_BYTES)?;
    let issued = runtime
        .issue(&admin, Duration::from_secs(3600), None)
        .await?;
    let key_id: KeyId = issued.metadata.key_id;
    let presented = runtime.derive_key(key_id)?;

    let derive = time(|| {
        black_box(runtime.derive_key(black_box(key_id))?);
        Ok(())
    })?;
    let temporary = time(|| {
        black_box(runtime.authenticate_presented(black_box(key_id), black_box(&presented))?);
        Ok(())
    })?;
    let administrator =
        time(|| {
            black_box(runtime.authenticate_presented(
                black_box(ADMIN_KEY_ID),
                black_box(&TEST_ADMIN_KEY_BYTES),
            )?);
            Ok(())
        })?;
    println!("case=derive_temporary_key ns={derive:.1}");
    println!("case=authenticate_temporary ns={temporary:.1}");
    println!("case=authenticate_administrator ns={administrator:.1}");

    drop(admin);
    runtime.shutdown_actor().await;
    Ok(())
}
