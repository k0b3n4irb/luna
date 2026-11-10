//! `luna audio-check` — thin front of `luna_api::audio_check`.

use std::path::Path;
use std::process::ExitCode;

/// Run the check, print the report. Exit `0` clean, `1` a fault of a
/// `--fail-on` kind, `2` a usage or I/O error.
pub(crate) fn run_audio_check(
    wav: &Path,
    voices_dir: Option<&Path>,
    region: &str,
    fail_on: Option<&str>,
    thresholds: luna_api::AudioThresholds,
    json: bool,
) -> ExitCode {
    let report = luna_api::AudioRegion::parse(region)
        .and_then(|region| {
            let fail_on = match fail_on {
                Some(list) => luna_api::FaultKind::parse_list(list)?,
                None => luna_api::FaultKind::DEFAULT_FAIL_ON.to_vec(),
            };
            Ok(luna_api::AudioCheckOptions {
                region,
                thresholds,
                fail_on,
            })
        })
        .and_then(|opts| luna_api::audio_check_files(wav, voices_dir, &opts));
    match report {
        Ok(r) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
            } else {
                println!("{}", r.render_text());
            }
            ExitCode::from(u8::from(r.verdict == "faults"))
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
