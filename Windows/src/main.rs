mod core;
mod plane;
mod platform;
mod runtime;

use runtime::{
    render_test::{RenderTestConfig, run_render_test},
    udp_render::{UdpRenderConfig, run_udp_render},
};

fn parse_arg(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("render-test") => {
            let config = RenderTestConfig {
                device_substring: parse_arg(&args, "--device"),
                seconds: parse_arg(&args, "--seconds")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(15),
                tone_hz: parse_arg(&args, "--tone-hz")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(440.0),
                source_ppm: parse_arg(&args, "--source-ppm")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.0),
            };
            run_render_test(config)
        }

        Some("udp-render") => {
            let config = UdpRenderConfig {
                bind: parse_arg(&args, "--bind")
                    .unwrap_or_else(|| "0.0.0.0:32124".to_string()),
                device_substring: parse_arg(&args, "--device"),
                seconds: parse_arg(&args, "--seconds")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(30),
            };
            run_udp_render(config)
        }

        _ => {
            eprintln!(
                "usage:\n  \
                 s24relay render-test [--device Realtek] [--seconds 15] [--tone-hz 440] [--source-ppm 0]\n  \
                 s24relay udp-render [--bind 0.0.0.0:32124] [--device Realtek] [--seconds 30]"
            );
            Ok(())
        }
    }
}
