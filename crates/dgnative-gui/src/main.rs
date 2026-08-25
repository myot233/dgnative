//! `dgnative-gui`: desktop control surface for the DG-LAB Coyote pulse host 3.0.
//!
//! Same safety posture as the CLI: the soft limit defaults to 20, both channels
//! start at 0, and nothing is sent to the device until you raise a strength.

mod device;
mod state;
mod ui;

use std::time::Duration;

use clap::Parser;
use gpui::{Application, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};

use crate::state::UiState;

#[derive(Parser)]
#[command(
    name = "dgnative-gui",
    version,
    about = "Desktop control surface for the DG-LAB Coyote pulse host 3.0"
)]
struct Args {
    /// Soft strength limit written to the device on connect
    #[arg(long, short = 'L', default_value_t = 20)]
    limit: u8,

    /// Timeout in seconds for scanning / connecting to a device
    #[arg(long, short = 't', default_value_t = 15)]
    timeout: u64,

    /// Id of the device to connect to (a prefix is enough); omit to take the
    /// strongest signal
    #[arg(long, short = 'D')]
    device: Option<String>,

    /// Do not touch any hardware; drive the window against a simulator
    #[arg(long)]
    offline: bool,
}

fn main() {
    let args = Args::parse();
    let options = device::Options {
        offline: args.offline,
        device: args.device,
        timeout: Duration::from_secs(args.timeout),
        limit: args.limit,
    };

    Application::new().run(move |cx| {
        ui::bind_keys(cx);

        let (tx, rx) = futures::channel::mpsc::unbounded();
        let state = UiState::new(options.limit);
        let handle = device::spawn(options, tx);

        let bounds = Bounds::centered(None, size(px(720.), px(520.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("dgnative".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| ui::Dashboard::new(state, handle, rx, window, cx)),
        )
        .expect("open window");
        cx.activate(true);
    });
}
