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
            Ok(Box::new(rms_product_app::RmsProductApp::new(
                main_thread_token,
                creation_context,
                Some(rms_product_app::DEFAULT_RECORDING_URL),
                None,
            )?))
        }),
    )?;

    Ok(())
}
