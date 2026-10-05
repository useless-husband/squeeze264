//! Conformance checking against external decoders. The encoder's own
//! reconstruction must equal, byte for byte, what an independent decoder
//! produces from the bitstream. ffmpeg is run as a separate process; it is
//! never linked into the encoder.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoder {
    /// ffmpeg's software H.264 decoder (libavcodec).
    FfmpegSoftware,
    /// Apple VideoToolbox through ffmpeg's hwaccel, with the hardware
    /// surface format forced so a silent software fallback is impossible.
    VideoToolbox,
}

impl Decoder {
    pub fn name(&self) -> &'static str {
        match self {
            Decoder::FfmpegSoftware => "ffmpeg (libavcodec h264)",
            Decoder::VideoToolbox => "Apple VideoToolbox (via ffmpeg hwaccel)",
        }
    }
}

/// Locates ffmpeg: $FFMPEG, then PATH, then the usual Homebrew location.
pub fn find_ffmpeg() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("FFMPEG") {
        candidates.push(p.into());
    }
    candidates.push("ffmpeg".into());
    candidates.push("/opt/homebrew/bin/ffmpeg".into());
    candidates.push("/usr/local/bin/ffmpeg".into());
    candidates.into_iter().find(|c| {
        Command::new(c)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Whether this ffmpeg build offers the VideoToolbox hwaccel.
pub fn has_videotoolbox(ffmpeg: &Path) -> bool {
    Command::new(ffmpeg)
        .args(["-hide_banner", "-hwaccels"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("videotoolbox"))
        .unwrap_or(false)
}

#[derive(Debug, Default)]
pub struct DecodeReport {
    /// Frames the decoder produced.
    pub decoded_frames: usize,
    /// Indices of frames that differ from the expected reconstruction.
    pub mismatches: Vec<usize>,
    /// First differing position: (frame, plane, x, y, expected, got).
    pub first_diff: Option<(usize, usize, usize, usize, u8, u8)>,
    /// Everything the decoder printed at warning level or above.
    pub stderr: String,
    pub exit_ok: bool,
    /// The hardware decoder could not be checked: it produced no frame at
    /// all (VideoToolbox rejects pictures smaller than 64x64), or this is a
    /// virtual machine, where frames come back through a paravirtual
    /// surface path that alters chroma samples. This is "not checked"
    /// rather than a mismatch.
    pub hw_refused: bool,
}

impl DecodeReport {
    pub fn bit_exact(&self, expected_frames: usize) -> bool {
        self.exit_ok
            && self.decoded_frames == expected_frames
            && self.mismatches.is_empty()
            && self.stderr.trim().is_empty()
    }
}

/// Decodes `stream` (Annex B or MP4) and compares each output frame with
/// the next `frame_bytes` bytes produced by `expected`.
pub fn check_decode(
    ffmpeg: &Path,
    decoder: Decoder,
    stream: &Path,
    width: usize,
    height: usize,
    mut expected: impl Read,
) -> io::Result<DecodeReport> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-nostdin", "-hide_banner", "-v", "warning"]);
    if decoder == Decoder::VideoToolbox {
        cmd.args(["-hwaccel", "videotoolbox", "-hwaccel_output_format", "videotoolbox_vld"]);
    }
    cmd.arg("-i").arg(stream);
    if decoder == Decoder::VideoToolbox {
        cmd.args(["-vf", "hwdownload,format=nv12,format=yuv420p"]);
    }
    cmd.args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"]);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut err_pipe = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err_pipe.read_to_string(&mut s);
        s
    });
    let mut out = child.stdout.take().unwrap();
    let frame_bytes = width * height * 3 / 2;
    let mut got = vec![0u8; frame_bytes];
    let mut want = vec![0u8; frame_bytes];
    let mut report = DecodeReport::default();
    let mut compare = || -> io::Result<()> {
        loop {
            match read_full(&mut out, &mut got)? {
                0 => break,
                n if n < frame_bytes => {
                    report.mismatches.push(report.decoded_frames);
                    break;
                }
                _ => {}
            }
            let idx = report.decoded_frames;
            report.decoded_frames += 1;
            if read_full(&mut expected, &mut want)? < frame_bytes {
                report.mismatches.push(idx);
                continue;
            }
            if got != want {
                report.mismatches.push(idx);
                if report.first_diff.is_none() {
                    let pos = got.iter().zip(&want).position(|(a, b)| a != b).unwrap();
                    let (plane, off, pw) = if pos < width * height {
                        (0, pos, width)
                    } else if pos < width * height * 5 / 4 {
                        (1, pos - width * height, width / 2)
                    } else {
                        (2, pos - width * height * 5 / 4, width / 2)
                    };
                    report.first_diff = Some((idx, plane, off % pw, off / pw, want[pos], got[pos]));
                }
            }
        }
        Ok(())
    };
    if let Err(e) = compare() {
        // Do not leave the decoder process behind when reading fails.
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    drop(out);
    report.exit_ok = child.wait()?.success();
    report.stderr = err_thread.join().unwrap_or_default();
    report.hw_refused = decoder == Decoder::VideoToolbox
        && (is_paravirtual(&report.stderr)
            || (report.decoded_frames == 0
                && (!report.exit_ok || report.stderr.contains("hwaccel initialisation returned error"))));
    Ok(report)
}

/// True when the hardware download ran on a virtual machine. GitHub's
/// macOS runners print "IOServiceMatchingfailed for:
/// AppleM2ScalerParavirtDriver" from IOKit; the frames that come back on
/// that path were seen to differ from real hardware in chroma for the same
/// stream, so they say nothing about the encoder.
fn is_paravirtual(stderr: &str) -> bool {
    stderr
        .lines()
        .any(|l| l.contains("IOServiceMatchingfailed for:") && l.contains("Paravirt"))
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::is_paravirtual;

    #[test]
    fn only_the_vm_driver_notice_marks_a_virtual_machine() {
        assert!(is_paravirtual(
            "IOServiceMatchingfailed for: AppleM2ScalerParavirtDriver\n"
        ));
        assert!(!is_paravirtual(
            "[h264 @ 0x1] error while decoding MB 3 4, bytestream -5"
        ));
        assert!(!is_paravirtual(""));
    }
}
