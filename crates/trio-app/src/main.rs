mod app;
mod cli;
mod engine;
mod export_job;
mod gpu;
mod jobs;
mod panels;
mod preview;
mod timeline;

use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,wgpu=warn,naga=warn")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(first) = args.first() {
        if matches!(
            first.as_str(),
            "new" | "sync" | "export" | "grade" | "probe"
        ) {
            return cli::run(&args);
        }
    }
    // A project file or a shoot folder to open at start.
    let project_path = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(std::path::PathBuf::from)
        .find(|p| p.is_dir() || p.extension().map(|e| e == "json").unwrap_or(false));
    let start_at = args
        .iter()
        .position(|a| a == "--at")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<f64>().ok());
    let autoplay = args.iter().any(|a| a == "--play");
    let autoexport = args.iter().any(|a| a == "--export");
    // `--screenshot FILE [--step N]`: draw the loaded project, save the window
    // as a PNG and quit. Used for the README pictures.
    let screenshot = args
        .iter()
        .position(|a| a == "--screenshot")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from);
    let step = args
        .iter()
        .position(|a| a == "--step")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<usize>().ok())
        .and_then(|n| panels::Step::ALL.get(n.wrapping_sub(1)).copied());

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("trio-capture")
            .with_inner_size([1560.0, 980.0])
            .with_min_inner_size([1000.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "trio-capture",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::App::new(
                cc,
                project_path,
                start_at,
                autoplay,
                autoexport,
                screenshot,
                step,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
