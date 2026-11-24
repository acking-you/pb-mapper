//! Application-wide power policy. The local request belongs to the runtime,
//! independently of any tunnel, viewer, or window. A configured Windows
//! service also owns its own request for the time it hosts without the app.
use super::*;
use pb_mapper_core::power::SystemAwake;

pub(crate) async fn restore_keep_awake(handle: &PbMapperHandle) {
    let _host = handle.desktop.host.lock().await;
    let enabled = handle.state.lock().await.get_config_status().keep_awake;
    let Some(enabled) = enabled else {
        let mut state = handle.state.lock().await;
        state.awake = None;
        state.awake_error = None;
        return;
    };
    let result = apply(handle, enabled, false).await;
    if let Err(error) = result {
        tracing::warn!(event = "keep_awake_failed", %error);
        handle.state.lock().await.awake_error = Some(error.to_string());
    }
}

pub(super) async fn set_keep_awake(
    handle: &PbMapperHandle,
    enabled: bool,
) -> Result<Value, CtlError> {
    // Host enable/resume also copies the current preference to the service.
    // Serialize the two operations so a start cannot restore a stale value.
    let _host = handle.desktop.host.lock().await;
    let result = apply(handle, enabled, true).await;
    if let Err(error) = &result {
        let mut state = handle.state.lock().await;
        if state.get_config_status().keep_awake == Some(enabled) {
            state.awake_error = Some(error.to_string());
        }
    }
    crate::events::emit(
        crate::events::ChangeKind::Config,
        None,
        crate::ctl::Origin::Ui,
    );
    result.map(|()| json!({}))
}

async fn apply(handle: &PbMapperHandle, enabled: bool, save: bool) -> Result<(), CtlError> {
    let need_request = enabled && handle.state.lock().await.awake.is_none();
    let acquired = if need_request {
        Some(
            SystemAwake::acquire()
                .await
                .map_err(|failure| err(format!("Could not prevent idle sleep: {failure}")))?,
        )
    } else {
        None
    };
    {
        let mut state = handle.state.lock().await;
        if save {
            state.save_keep_awake(enabled)?;
        }
        if !enabled {
            state.awake = None;
        } else if acquired.is_some() {
            state.awake = acquired;
        }
        state.awake_error = None;
    }
    #[cfg(windows)]
    background::sync_keep_awake(handle, enabled).await?;
    Ok(())
}
