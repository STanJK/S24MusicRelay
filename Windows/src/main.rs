mod core;
mod plane;
mod platform;
mod runtime;

#[cfg(windows)]
use runtime::chirp_align::{
    run_chirp_align, ChirpAlignConfig,
};
#[cfg(windows)]
use runtime::dual_output_gui::{
    run_dual_output_gui, DualOutputGuiConfig,
};

use runtime::{
    render_test::{RenderTestConfig, run_render_test},
    udp_render::{UdpRenderConfig, run_udp_render},
    udp_send_live::{UdpSendLiveConfig, run_udp_send_live},
    udp_send_test::{UdpSendTestConfig, run_udp_send_test},
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





        #[cfg(windows)]
        Some("chirp-align") => {
            let target = parse_arg(&args, "--target")
                .ok_or("chirp-align requires --target <phone-ip>:32124")?;
            let local_device_substring = parse_arg(&args, "--device")
                .unwrap_or_else(|| "Realtek".to_string());
            let mic_device_substring = parse_arg(&args, "--mic")
                .unwrap_or_else(|| "USB_MIC".to_string());

            run_chirp_align(ChirpAlignConfig {
                target,
                local_device_substring,
                mic_device_substring,
            })
        }

        #[cfg(windows)]
        Some("dual-output-gui") => {
            let target = parse_arg(&args, "--target")
                .ok_or("dual-output-gui requires --target <phone-ip>:32124")?;
            let sacrificial_substring = parse_arg(&args, "--sacrificial")
                .ok_or("dual-output-gui requires --sacrificial <default-endpoint-substring>")?;
            let local_device_substring = parse_arg(&args, "--device")
                .unwrap_or_else(|| "Realtek".to_string());
            let mic_device_substring = parse_arg(&args, "--mic")
                .unwrap_or_else(|| "USB_MIC".to_string());

            run_dual_output_gui(DualOutputGuiConfig {
                target,
                sacrificial_substring,
                local_device_substring,
                mic_device_substring,
            })
            .map_err(|error| -> Box<dyn std::error::Error> {
                error.into()
            })
        }

        Some("udp-send-live") => {
            let target = parse_arg(&args, "--target")
                .ok_or("udp-send-live requires --target <phone-ip>:32124")?;
            let sacrificial_substring = parse_arg(&args, "--sacrificial")
                .ok_or("udp-send-live requires --sacrificial <default-endpoint-substring>")?;
            let config = UdpSendLiveConfig {
                target,
                sacrificial_substring,
                seconds: parse_arg(&args, "--seconds")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(30),
                gain: parse_arg(&args, "--gain")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1.0),
            };
            run_udp_send_live(config)
        }

        Some("udp-send-test") => {
            let target = parse_arg(&args, "--target")
                .ok_or("udp-send-test requires --target <phone-ip>:32124")?;
            let config = UdpSendTestConfig {
                target,
                seconds: parse_arg(&args, "--seconds")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(30),
                tone_hz: parse_arg(&args, "--tone-hz")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(440.0),
                gain: parse_arg(&args, "--gain")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.15),
            };
            run_udp_send_test(config)
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
                 s24relay udp-render [--bind 0.0.0.0:32124] [--device Realtek] [--seconds 30]\n  \
                 s24relay udp-send-test --target <phone-ip>:32124 [--seconds 30] [--tone-hz 440] [--gain 0.15]\n  \
                 s24relay udp-send-live --target <phone-ip>:32124 --sacrificial <substring> [--seconds 30] [--gain 1.0]\n  \
                 s24relay dual-output-gui --target <phone-ip>:32124 --sacrificial <substring> [--device Realtek] [--mic USB_MIC]\n  \
                 s24relay chirp-align --target <phone-ip>:32124 [--device Realtek] [--mic USB_MIC]"
            );
            Ok(())
        }
    }
}
