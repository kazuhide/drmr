use super::*;
use clap::error::ErrorKind;
use drmr_core::inspection::{OutputFormat, TargetProtection};

fn parse(arguments: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("drmr").chain(arguments.iter().copied())).unwrap()
}

#[test]
fn explicit_output_profiles_and_encryption_have_distinct_roles() {
    for (profile, expected) in [
        ("mp4", OutputFormat::Mp4),
        ("dash", OutputFormat::Dash),
        ("hls-fmp4", OutputFormat::Hls),
        ("hls-ts", OutputFormat::HlsTs),
    ] {
        let destination = if profile == "mp4" {
            "--output-file"
        } else {
            "--output-dir"
        };
        let Command::Convert(args) = parse(&[
            "convert",
            "original.mp4",
            "--encryption",
            "clear",
            "--output-format",
            profile,
            destination,
            "new-result",
        ])
        .command
        else {
            panic!("expected convert")
        };
        assert_eq!(args.format, Some(expected));
        assert_eq!(args.to, Some(TargetProtection::Clear));
    }
    let error = Cli::try_parse_from([
        "drmr",
        "convert",
        "source.m3u8",
        "--encryption",
        "clear",
        "--output-format",
        "hls",
        "--output-dir",
        "result",
    ])
    .err()
    .unwrap();
    assert_eq!(error.kind(), ErrorKind::ValueValidation);
}

#[test]
fn old_convert_names_are_not_silent_aliases() {
    for (name, value) in [
        ("--to", "clear"),
        ("--format", "dash"),
        ("--output", "result"),
        ("--outputs", "outputs.json"),
        ("--fetch-policy", "fetch.json"),
        ("--plan", "plan.json"),
        ("--measure", ""),
        ("--dash-validator-bin", "php"),
    ] {
        let mut arguments = vec![
            "drmr",
            "convert",
            "original.mp4",
            "--encryption",
            "clear",
            "--output-file",
            "new.mp4",
            name,
        ];
        if !value.is_empty() {
            arguments.push(value);
        }
        let error = Cli::try_parse_from(arguments).err().unwrap();
        assert_eq!(error.kind(), ErrorKind::UnknownArgument, "{name}");
    }
}

#[test]
fn output_config_can_supply_its_destination_but_not_a_single_file() {
    let Command::Convert(args) =
        parse(&["convert", "source.m3u8", "--output-config", "outputs.json"]).command
    else {
        panic!("expected convert")
    };
    assert!(args.destination().is_none());
    let error = Cli::try_parse_from([
        "drmr",
        "convert",
        "source.m3u8",
        "--output-config",
        "outputs.json",
        "--output-file",
        "result.mp4",
    ])
    .err()
    .unwrap();
    assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
}

#[test]
fn destination_kinds_are_mutually_exclusive() {
    let error = Cli::try_parse_from([
        "drmr",
        "convert",
        "original.mp4",
        "--encryption",
        "clear",
        "--output-file",
        "result.mp4",
        "--output-dir",
        "result",
    ])
    .err()
    .unwrap();
    assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
}

#[test]
fn statistics_and_report_file_do_not_require_json_stdout() {
    let Command::Convert(args) = parse(&[
        "convert",
        "original.mp4",
        "--encryption",
        "clear",
        "--output-file",
        "result.mp4",
        "--stats",
        "--report-file",
        "result.json",
    ])
    .command
    else {
        panic!("expected convert")
    };
    assert!(args.measure && !args.json);
    assert_eq!(args.report_file.as_deref(), Some(Path::new("result.json")));
}

#[test]
fn saved_intent_cannot_be_overridden_by_new_output_flags() {
    for (name, value) in [
        ("--output-dir", "result"),
        ("--encryption", "clear"),
        ("--manifest-profile", "base"),
    ] {
        let error =
            Cli::try_parse_from(["drmr", "convert", "--plan-file", "plan.json", name, value])
                .err()
                .unwrap();
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict, "{name}");
    }
}

#[test]
fn direct_named_verification_and_external_checks_are_available() {
    let Command::Verify(args) = parse(&[
        "verify",
        "result",
        "--source",
        "source/input.m3u8",
        "--output-config",
        "outputs.json",
        "--external-manifest-check",
        "--php-bin",
        "php",
    ])
    .command
    else {
        panic!("expected verify")
    };
    assert!(args.outputs.is_some() && args.manifest.external_validator);
    assert!(!args.manifest.manifest_only);
}

#[test]
fn saved_url_plans_keep_the_download_destination_explicit() {
    let Command::Plan(args) = parse(&[
        "plan",
        "https://example.test/master.m3u8",
        "--download-dir",
        "source",
        "--output-config",
        "outputs.json",
        "--save",
        "plan.json",
    ])
    .command
    else {
        panic!("expected plan")
    };
    assert_eq!(args.download_dir.as_deref(), Some(Path::new("source")));
}

#[test]
fn doctor_accepts_output_config_without_an_internal_route_name() {
    assert!(matches!(
        parse(&["doctor", "--output-config", "outputs.json"]).command,
        Command::Doctor {
            outputs: Some(_),
            ..
        }
    ));
}

#[test]
fn plan_decode_requires_saved_complete_source_checks() {
    let error = Cli::try_parse_from([
        "drmr",
        "plan",
        "original.mp4",
        "--encryption",
        "clear",
        "--decode",
    ])
    .err()
    .unwrap();
    assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    let Command::Plan(args) = parse(&[
        "plan",
        "original.mp4",
        "--encryption",
        "clear",
        "--output-file",
        "new.mp4",
        "--save",
        "plan.json",
        "--decode",
    ])
    .command
    else {
        panic!("expected plan")
    };
    assert!(args.decoding.decode);
}
