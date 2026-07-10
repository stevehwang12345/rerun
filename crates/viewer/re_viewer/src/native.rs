/// Used by `eframe` to decide where to store the app state.
#[cfg(not(feature = "rms_white_label"))]
pub const APP_ID: &str = "rerun";

#[cfg(feature = "rms_white_label")]
pub const APP_ID: &str = "rust-rms-viewer";

#[cfg(not(feature = "rms_white_label"))]
const WINDOW_TITLE: &str = "Rerun";

#[cfg(feature = "rms_white_label")]
const WINDOW_TITLE: &str = "Rust-RMS Viewer";

type DynError = Box<dyn std::error::Error + Send + Sync>;

type AppCreator =
    Box<dyn FnOnce(&eframe::CreationContext<'_>) -> Result<Box<dyn eframe::App>, DynError>>;

// NOTE: the name of this function is hard-coded in `crates/top/rerun/src/crash_handler.rs`!
pub fn run_native_app(
    // `eframe::run_native` may only be called on the main thread.
    _: crate::MainThreadToken,
    app_creator: AppCreator,
    force_wgpu_backend: Option<&str>,
) -> eframe::Result {
    if crate::docker_detection::is_docker() {
        re_log::warn_once!(
            "It looks like you are running the Rerun Viewer inside a Docker container. This is not officially supported, and may lead to performance issues and bugs. See https://github.com/rerun-io/rerun/issues/6835 for more.",
        );
    }

    let native_options = eframe_options(force_wgpu_backend);

    eframe::run_native(
        WINDOW_TITLE,
        native_options,
        Box::new(move |cc| {
            crate::customize_eframe_and_setup_renderer(cc)?;
            app_creator(cc)
        }),
    )
}

pub fn eframe_options(force_wgpu_backend: Option<&str>) -> eframe::NativeOptions {
    re_tracing::profile_function!();
    let os = egui::os::OperatingSystem::default();
    let custom_window_decorations = re_ui::supports_custom_decorations(os);
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id(APP_ID) // Controls where on disk the app state is persisted
            .with_decorations(!custom_window_decorations) // Maybe hide the OS-specific "chrome" around the window
            .with_fullsize_content_view(re_ui::fullsize_content(os))
            .with_icon(icon_data())
            .with_inner_size(default_inner_size())
            .with_min_inner_size(minimum_inner_size())
            .with_title_shown(!re_ui::fullsize_content(os))
            .with_titlebar_buttons_shown(!custom_window_decorations)
            .with_titlebar_shown(!re_ui::fullsize_content(os))
            .with_transparent(custom_window_decorations), // To have rounded corners without decorations we need transparency on Linux. On Windows this mostly affects resizing which looks a bit better with this.

        renderer: eframe::Renderer::Wgpu,
        wgpu_options: crate::wgpu_options(force_wgpu_backend),
        depth_buffer: 0,
        multisampling: 0, // the 3D views do their own MSAA

        ..Default::default()
    }
}

fn default_inner_size() -> [f32; 2] {
    if cfg!(feature = "rms_white_label") {
        [1280.0, 860.0]
    } else {
        [1600.0, 1200.0]
    }
}

fn minimum_inner_size() -> [f32; 2] {
    if cfg!(feature = "rms_white_label") {
        [420.0, 360.0]
    } else {
        [320.0, 450.0]
    }
}

#[cfg(feature = "rms_white_label")]
fn icon_data() -> egui::IconData {
    re_tracing::profile_function!();

    const SIZE: u32 = 32;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);

    for y in 0..SIZE {
        for x in 0..SIZE {
            let border = x < 2 || y < 2 || x >= SIZE - 2 || y >= SIZE - 2;
            let diagonal = x.abs_diff(y) <= 1 || (SIZE - 1 - x).abs_diff(y) <= 1;
            let core = (10..22).contains(&x) && (10..22).contains(&y);
            let color = if border {
                [15, 23, 42, 255]
            } else if diagonal {
                [56, 189, 248, 255]
            } else if core {
                [34, 197, 94, 255]
            } else {
                [248, 250, 252, 255]
            };
            rgba.extend_from_slice(&color);
        }
    }

    egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}

#[cfg(not(feature = "rms_white_label"))]
fn icon_data() -> egui::IconData {
    re_tracing::profile_function!();

    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            let app_icon_png_bytes = include_bytes!("../data/app_icon_mac.png");
        } else if #[cfg(target_os = "windows")] {
            let app_icon_png_bytes = include_bytes!("../data/app_icon.png");
        } else {
            // Use the same icon for X11 as for Windows, at least for now.
            let app_icon_png_bytes = include_bytes!("../data/app_icon.png");
        }
    };

    // We include the .png with `include_bytes`. If that fails, things are extremely broken.
    match eframe::icon_data::from_png_bytes(app_icon_png_bytes) {
        Ok(icon_data) => icon_data,
        Err(err) => {
            #[cfg(debug_assertions)]
            panic!("Failed to load app icon: {err}");

            #[cfg(not(debug_assertions))]
            {
                re_log::warn!("Failed to load app icon: {err}");
                Default::default()
            }
        }
    }
}
