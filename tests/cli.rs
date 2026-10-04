//! End-to-end tests of the command line tool.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_squeeze264"))
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("squeeze264-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// Removes the files a test created (kept when the test fails, for debugging).
fn cleanup(names: &[&str]) {
    for n in names {
        std::fs::remove_file(tmp(n)).ok();
    }
    std::fs::remove_dir(tmp("x").parent().unwrap()).ok();
}

fn gen(name: &str, size: &str, frames: u32) -> PathBuf {
    let path = tmp(name);
    let out = bin()
        .args([
            "gen",
            path.to_str().unwrap(),
            "--size",
            size,
            "--frames",
            &frames.to_string(),
            "-q",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    path
}

#[test]
fn encode_writes_all_requested_outputs() {
    let src = gen("a.y4m", "96x80", 12);
    let (h264, mp4, recon, md5, stats) = (
        tmp("a.h264"),
        tmp("a.mp4"),
        tmp("a-recon.y4m"),
        tmp("a.md5"),
        tmp("a.json"),
    );
    let out = bin()
        .arg("encode")
        .arg(&src)
        .args(["-o", h264.to_str().unwrap(), "-o", mp4.to_str().unwrap()])
        .args(["--recon", recon.to_str().unwrap(), "--framemd5", md5.to_str().unwrap()])
        .args(["--stats", stats.to_str().unwrap(), "--viz-frame", "3", "--qp", "30"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("12 frames 96x80"), "{text}");
    assert!(text.contains("PSNR"), "{text}");

    let stream = std::fs::read(&h264).unwrap();
    assert_eq!(&stream[..5], &[0, 0, 0, 1, 0x67], "Annex B must start with the SPS");
    let file = std::fs::read(&mp4).unwrap();
    assert_eq!(&file[4..8], b"ftyp");
    // The reconstruction is a Y4M file of the same geometry as the input.
    assert_eq!(
        std::fs::metadata(&recon).unwrap().len(),
        std::fs::metadata(&src).unwrap().len()
    );
    let md5s = std::fs::read_to_string(&md5).unwrap();
    assert_eq!(md5s.lines().count(), 12);
    assert!(md5s.lines().all(|l| l.len() == 32));
    let json = std::fs::read_to_string(&stats).unwrap();
    assert_eq!(json.matches("\"type\":").count(), 12);
    assert!(json.contains("\"viz\":{\"frame\":3"));
    assert_eq!(
        json.matches("\"t\":").count(),
        6 * 5,
        "one record per macroblock of the viz frame"
    );
    // Balanced braces and brackets: cheap structural check without a JSON parser.
    for (open, close) in [('{', '}'), ('[', ']')] {
        assert_eq!(json.matches(open).count(), json.matches(close).count());
    }
    cleanup(&["a.y4m", "a.h264", "a.mp4", "a-recon.y4m", "a.md5", "a.json"]);
}

/// The per-frame MD5 list must equal what `ffmpeg -f framemd5` computes
/// from the bitstream: anyone can repeat the conformance claim by hand.
#[test]
fn framemd5_matches_ffmpeg() {
    let Some(ffmpeg) = squeeze264::verify::find_ffmpeg() else {
        eprintln!("SKIP framemd5: ffmpeg not found");
        return;
    };
    let src = gen("b.y4m", "128x96", 15);
    let (h264, md5) = (tmp("b.h264"), tmp("b.md5"));
    let out = bin()
        .arg("encode")
        .arg(&src)
        .args([
            "-o",
            h264.to_str().unwrap(),
            "--framemd5",
            md5.to_str().unwrap(),
            "-q",
            "--bitrate",
            "200",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let ours: Vec<String> = std::fs::read_to_string(&md5)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    let ff = Command::new(ffmpeg)
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(&h264)
        .args(["-f", "framemd5", "-"])
        .output()
        .unwrap();
    assert!(
        ff.status.success() && ff.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&ff.stderr)
    );
    let theirs: Vec<String> = String::from_utf8_lossy(&ff.stdout)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.rsplit(',').next().unwrap().trim().to_string())
        .collect();
    assert_eq!(ours.len(), 15);
    assert_eq!(ours, theirs);
    cleanup(&["b.y4m", "b.h264", "b.md5"]);
}

#[test]
fn check_command_reports_bit_exactness() {
    if squeeze264::verify::find_ffmpeg().is_none() {
        eprintln!("SKIP check command: ffmpeg not found");
        return;
    }
    let src = gen("c.y4m", "176x144", 10);
    let out = bin().arg("check").arg(&src).args(["--qp", "25"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{text}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("PASS  ffmpeg"), "{text}");
    assert!(!text.contains("FAIL"), "{text}");
    assert!(text.contains("bit-exact"), "{text}");
    cleanup(&["c.y4m"]);
}

#[test]
fn errors_are_reported_with_exit_status_1() {
    let cases: Vec<Vec<String>> = vec![
        vec![
            "encode".into(),
            "/nonexistent/input.y4m".into(),
            "-o".into(),
            tmp("x.h264").display().to_string(),
        ],
        vec!["encode".into(), tmp("missing.y4m").display().to_string()],
        vec!["frobnicate".into(), "x".into()],
        vec![
            "encode".into(),
            "x.y4m".into(),
            "--qp".into(),
            "99".into(),
            "-o".into(),
            "y.h264".into(),
        ],
        vec!["encode".into(), "x.y4m".into(), "--no-such-flag".into()],
        vec!["encode".into()],
    ];
    for args in cases {
        let out = bin().args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).starts_with("error: "), "{args:?}");
    }
    // Not a Y4M file.
    let junk = tmp("junk.y4m");
    std::fs::write(&junk, b"RIFF\n not video").unwrap();
    let out = bin()
        .arg("encode")
        .arg(&junk)
        .args(["-o", tmp("j.h264").to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a Y4M file"));
    // Odd picture sizes cannot be represented in 4:2:0.
    let odd = tmp("odd.y4m");
    let mut data = b"YUV4MPEG2 W17 H16 F25:1\nFRAME\n".to_vec();
    data.extend(vec![0u8; 17 * 16 + 2 * 9 * 8]);
    std::fs::write(&odd, data).unwrap();
    let out = bin()
        .arg("encode")
        .arg(&odd)
        .args(["-o", tmp("o.h264").to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("even width and height"));
    // Help works and exits successfully.
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("USAGE"));
    cleanup(&["junk.y4m", "odd.y4m", "x.h264", "j.h264", "o.h264"]);
}

#[test]
fn truncated_input_is_reported() {
    let src = gen("d.y4m", "64x48", 3);
    let data = std::fs::read(&src).unwrap();
    let cut = tmp("d-cut.y4m");
    std::fs::write(&cut, &data[..data.len() - 100]).unwrap();
    let out = bin()
        .arg("encode")
        .arg(&cut)
        .args(["-o", tmp("d.h264").to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("d-cut.y4m"));
    cleanup(&["d.y4m", "d-cut.y4m", "d.h264"]);
}
