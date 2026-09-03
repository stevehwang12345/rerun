fn development_replay_context() -> rms_product_app::ReplayViewerContext {
    rms_product_app::ReplayViewerContext {
        project_id: "development".to_owned(),
        project_name: "RMS Development".to_owned(),
        device_id: "rerun-example".to_owned(),
        device_name: "Rerun Example".to_owned(),
        recording_id: "arkit-scenes".to_owned(),
        recording_name: "ARKit Scenes".to_owned(),
        replay_session_id: "native-development-replay".to_owned(),
        source_url: rms_product_app::DEFAULT_RECORDING_URL.to_owned(),
        captured_at_label: None,
        initial_timeline: String::new(),
        initial_fps: None,
        initial_cursor: None,
        initial_play_state: rms_product_app::ReplayInitialPlayState::Paused,
        initial_speed: 1.0,
        initial_loop: rms_product_app::ReplayInitialLoop::default(),
        topics: Vec::new(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    re_log::setup_logging();
    re_crash_handler::install_crash_handlers(re_viewer::build_info());

    let main_thread_token = re_viewer::MainThreadToken::i_promise_i_am_on_the_main_thread();
    let mut native_options = re_viewer::native::eframe_options(None);
    native_options.viewport = native_options.viewport.with_app_id("rms_product_app");

    eframe::run_native(
        "RMS Operations",
        native_options,
        Box::new(move |creation_context| {
            let mut app = rms_product_app::RmsProductApp::new(
                main_thread_token,
                creation_context,
                None,
                None,
            )?;
            app.apply_replay_context(development_replay_context());
            Ok(Box::new(app))
        }),
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::development_replay_context;

    #[test]
    fn native_launcher_starts_through_the_replay_service_context() {
        let context = development_replay_context();
        assert_eq!(context.source_url, rms_product_app::DEFAULT_RECORDING_URL);
        assert!(context.initial_timeline.is_empty());
        assert!(context.initial_cursor.is_none());
        assert!(context.initial_fps.is_none());
        assert_eq!(context.initial_speed, 1.0);
        assert_eq!(
            context.initial_play_state,
            rms_product_app::ReplayInitialPlayState::Paused
        );
    }
}
