mod lib;

use argh::FromArgs;
use cargo_metadata::camino::Utf8PathBuf;
use lib::{Profile, Target, build, default_build_dir};

/// Build the web-viewer.
#[derive(FromArgs)]
#[argh(subcommand, name = "build-web-viewer")]
pub struct Args {
    /// compile for release and run wasm-opt.
    ///
    /// Mutually exclusive with `--debug`.
    /// NOTE: --release also removes debug symbols which are otherwise useful for in-browser profiling.
    #[argh(switch)]
    release: bool,

    /// compile for debug and don't run wasm-opt.
    ///
    /// Mutually exclusive with `--release`.
    #[argh(switch)]
    debug: bool,

    /// keep debug symbols, even in release builds.
    /// This gives better callstacks on panics, and also allows for in-browser profiling of the Wasm.
    #[argh(switch, short = 'g')]
    debug_symbols: bool,

    /// target to build for.
    #[argh(option, short = 't', long = "target", default = "Target::Browser")]
    target: Target,

    /// set the output directory. This is a path relative to the cargo workspace root.
    #[argh(option, short = 'o', long = "out")]
    build_dir: Option<Utf8PathBuf>,

    /// cargo package containing the cdylib to build.
    #[argh(option, long = "package", default = "default_package()")]
    package: String,

    /// output stem passed to wasm-bindgen. Defaults to the cdylib target name.
    #[argh(option, long = "out-name")]
    out_name: Option<String>,

    /// comma-separated list of features to pass to the selected package.
    #[argh(option, short = 'F', long = "features")]
    features: Option<String>,

    /// whether to exclude default features from the selected package.
    #[argh(switch, long = "no-default-features")]
    no_default_features: bool,

    /// generate a cargo build timings report in `<target-dir>/cargo-timings/`.
    #[argh(switch)]
    timings: bool,
}

fn default_package() -> String {
    "re_viewer".to_owned()
}

pub fn main(args: Args) -> anyhow::Result<()> {
    let profile = if args.release && !args.debug {
        Profile::WebRelease
    } else if !args.release && args.debug {
        Profile::Debug
    } else {
        return Err(anyhow::anyhow!(
            "Exactly one of --release or --debug must be set"
        ));
    };

    let build_dir = args.build_dir.unwrap_or_else(default_build_dir);
    let features = args.features.unwrap_or_else(|| {
        if args.package == default_package() {
            "analytics".to_owned()
        } else {
            String::new()
        }
    });

    build(
        profile,
        args.debug_symbols,
        args.target,
        &build_dir,
        &args.package,
        args.out_name.as_deref(),
        args.no_default_features,
        &features,
        args.timings,
    )
}
