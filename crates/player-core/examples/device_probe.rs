//! Diagnostic: enumerate every output device and show what `Playback::open`
//! would do with a typical 44.1kHz stereo track on each one.
//!
//! Written to chase two live reports from the first Windows desktop test
//! (2026-08-15): "no usable audio output device" on some default devices but
//! not others, and very sped-up playback on the one that worked. Both point
//! at `playback::pick_config`, which this reproduces per-device -- including
//! the real cpal error that `PlayerError::NoOutputDevice` currently throws
//! away.
//!
//! Run: cargo run -p player-core --example device_probe

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{SampleFormat, StreamConfig};

const FILE_RATE: u32 = 44_100;
const FILE_CHANNELS: u16 = 2;

fn main() {
    let host = cpal::default_host();
    println!("host: {:?}\n", host.id());

    let default_name = host
        .default_output_device()
        .map(|d| d.to_string())
        .unwrap_or_else(|| "<none>".into());
    println!("default output device: {default_name}\n");

    let devices = match host.output_devices() {
        Ok(d) => d,
        Err(e) => {
            println!("output_devices() failed: {e}");
            return;
        }
    };

    for device in devices {
        let name = device.to_string();
        let is_default = name == default_name;
        println!(
            "=== {name}{} ===",
            if is_default { "   [DEFAULT]" } else { "" }
        );

        match device.default_output_config() {
            Ok(c) => println!(
                "  mix format (default_output_config): {} ch, {} Hz, {:?}",
                c.channels(),
                c.sample_rate(),
                c.sample_format()
            ),
            Err(e) => println!("  default_output_config() FAILED: {e}"),
        }

        let configs: Vec<_> = match device.supported_output_configs() {
            Ok(c) => c.collect(),
            Err(e) => {
                println!("  supported_output_configs() FAILED: {e}\n");
                continue;
            }
        };
        let channel_counts: std::collections::BTreeSet<_> =
            configs.iter().map(|c| c.channels()).collect();
        println!(
            "  supported_output_configs(): {} entries, channel counts offered: {:?}",
            configs.len(),
            channel_counts
        );

        // OLD behaviour (kept so the regression stays visible): search the
        // advertised configs for the file's own rate and channel count.
        let candidates: Vec<_> = configs
            .iter()
            .filter(|c| c.channels() == FILE_CHANNELS)
            .filter(|c| c.min_sample_rate() <= FILE_RATE && FILE_RATE <= c.max_sample_rate())
            .collect();
        let old_picked = candidates
            .iter()
            .find(|c| c.sample_format() == SampleFormat::F32)
            .or_else(|| {
                candidates
                    .iter()
                    .find(|c| c.sample_format() == SampleFormat::I16)
            })
            .map(|c| (*(*c)).with_sample_rate(FILE_RATE));

        match &old_picked {
            Some(c) => {
                let ok = try_build(&device, c).is_ok();
                println!(
                    "  OLD pick_config -> {} ch, {} Hz, {:?}  => build {}",
                    c.channels(),
                    c.sample_rate(),
                    c.sample_format(),
                    if ok {
                        "OK"
                    } else {
                        "FAILED (\"no usable audio output device\")"
                    }
                );
            }
            None => {
                let ratio = device
                    .default_output_config()
                    .map(|c| c.sample_rate() as f64 / FILE_RATE as f64)
                    .unwrap_or(f64::NAN);
                println!(
                    "  OLD pick_config -> no match, fell back to mix format => played at x{ratio:.3} speed"
                );
            }
        }

        // NEW behaviour: take the device's mix format, resample to it.
        let chosen = match device.default_output_config() {
            Ok(c) => {
                println!(
                    "  NEW device_config -> {} ch, {} Hz, {:?}  (audio resampled {FILE_RATE} -> {})",
                    c.channels(),
                    c.sample_rate(),
                    c.sample_format(),
                    c.sample_rate()
                );
                Some(c)
            }
            Err(e) => {
                println!("  NEW device_config -> FAILED: {e}");
                None
            }
        };

        // Actually try to open it, surfacing the real error text.
        if let Some(c) = chosen {
            match try_build(&device, &c) {
                Ok(()) => println!("  => build_output_stream OK"),
                Err(e) => println!("  => build_output_stream FAILED: {e}   (real cpal error)"),
            }
        }
        println!();
    }
}

fn try_build(device: &cpal::Device, c: &cpal::SupportedStreamConfig) -> Result<(), String> {
    let config: StreamConfig = c.config();
    match c.sample_format() {
        SampleFormat::F32 => device
            .build_output_stream(
                config,
                move |d: &mut [f32], _: &cpal::OutputCallbackInfo| d.fill(0.0),
                |e| eprintln!("  stream error cb: {e}"),
                None,
            )
            .map(|_| ())
            .map_err(|e| e.to_string()),
        SampleFormat::I16 => device
            .build_output_stream(
                config,
                move |d: &mut [i16], _: &cpal::OutputCallbackInfo| d.fill(0),
                |e| eprintln!("  stream error cb: {e}"),
                None,
            )
            .map(|_| ())
            .map_err(|e| e.to_string()),
        other => Err(format!("build_stream cannot feed sample format {other:?}")),
    }
}
