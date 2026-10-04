//! Command line front end: encode a Y4M file to H.264 (Annex B or MP4),
//! verify the result against external decoders, or generate a test clip.

use squeeze264::encoder::{Config, EncodedFrame, Encoder, FrameStats};
use squeeze264::frame::psnr;
use squeeze264::mb::{MbMode, MbRecord};
use squeeze264::md5::md5_hex;
use squeeze264::mp4::Mp4Writer;
use squeeze264::ratecontrol::RcMode;
use squeeze264::synth::{synth_frame, Pattern};
use squeeze264::verify::{check_decode, find_ffmpeg, has_videotoolbox, Decoder};
use squeeze264::y4m::{self, Y4mHeader, Y4mReader};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufReader, BufWriter, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

const USAGE: &str = "\
squeeze264 - H.264 Constrained Baseline encoder

USAGE:
  squeeze264 encode <in.y4m> -o <out.mp4|out.h264> [options]
  squeeze264 check  <in.y4m> [options]     encode, then decode with ffmpeg and
                                           VideoToolbox and compare bit for bit
  squeeze264 gen    <out.y4m> [--size WxH] [--frames N] [--fps N]
                              [--pattern moving|noise|still|extremes] [--seed N]

ENCODE OPTIONS:
  -o FILE              output; .mp4/.m4v/.mov = MP4, anything else = Annex B
                       (may be given twice to write both)
  --qp N               constant QP for P frames, 0..51 (default 28)
  --i-qp-offset N      I frames use QP - N (default 3)
  --bitrate KBPS       target average bitrate instead of constant QP
  --keyint N           IDR interval in frames (default 250)
  --frames N           stop after N frames
  --me-range N         integer motion search range (default 16)
  --subpel 0|1|2       integer / half / quarter sample motion (default 2)
  --no-partitions      16x16 motion only
  --no-sub8x8          no 8x4, 4x8, 4x4 sub-partitions
  --no-intra-in-p      no intra macroblocks in P frames
  --no-deblock         disable the in-loop deblocking filter
  --deblock A:B        deblocking alpha:beta offsets, each -6..6
  --chroma-qp-offset N -12..12
  --no-decimate        keep isolated small coefficients
  --recon FILE.y4m     write the encoder's reconstruction
  --framemd5 FILE      write one MD5 per reconstructed frame (ffmpeg framemd5 order)
  --stats FILE.json    write per-frame statistics
  --viz-frame N        include macroblock modes and vectors of frame N in the stats
  --verbose            one line per frame
  --quiet              no progress output
";

struct Options {
    input: PathBuf,
    outputs: Vec<PathBuf>,
    qp: u8,
    i_qp_offset: u8,
    bitrate: Option<u32>,
    keyint: usize,
    frames: Option<usize>,
    me_range: i32,
    subpel: u8,
    partitions: bool,
    sub8x8: bool,
    intra_in_p: bool,
    deblock: bool,
    deblock_offsets: (i8, i8),
    chroma_qp_offset: i8,
    decimate: bool,
    recon: Option<PathBuf>,
    recon_raw: Option<PathBuf>,
    framemd5: Option<PathBuf>,
    stats: Option<PathBuf>,
    viz_frame: Option<usize>,
    verbose: bool,
    quiet: bool,
    // gen
    size: (usize, usize),
    fps: u32,
    pattern: Pattern,
    seed: u64,
}

fn parse<T: std::str::FromStr>(name: &str, v: Option<String>) -> Result<T, String> {
    let v = v.ok_or_else(|| format!("{name} needs a value"))?;
    v.parse().map_err(|_| format!("bad value for {name}: {v}"))
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut o = Options {
        input: PathBuf::new(),
        outputs: Vec::new(),
        qp: 28,
        i_qp_offset: 3,
        bitrate: None,
        keyint: 250,
        frames: None,
        me_range: 16,
        subpel: 2,
        partitions: true,
        sub8x8: true,
        intra_in_p: true,
        deblock: true,
        deblock_offsets: (0, 0),
        chroma_qp_offset: 0,
        decimate: true,
        recon: None,
        recon_raw: None,
        framemd5: None,
        stats: None,
        viz_frame: None,
        verbose: false,
        quiet: false,
        size: (352, 288),
        fps: 30,
        pattern: Pattern::Moving,
        seed: 1,
    };
    let mut have_input = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" | "--output" => o.outputs.push(parse::<String>("-o", args.next())?.into()),
            "--qp" => o.qp = parse("--qp", args.next())?,
            "--i-qp-offset" => o.i_qp_offset = parse("--i-qp-offset", args.next())?,
            "--bitrate" => o.bitrate = Some(parse("--bitrate", args.next())?),
            "--keyint" => o.keyint = parse("--keyint", args.next())?,
            "--frames" => o.frames = Some(parse("--frames", args.next())?),
            "--me-range" => o.me_range = parse("--me-range", args.next())?,
            "--subpel" => o.subpel = parse("--subpel", args.next())?,
            "--no-partitions" => o.partitions = false,
            "--no-sub8x8" => o.sub8x8 = false,
            "--no-intra-in-p" => o.intra_in_p = false,
            "--no-deblock" => o.deblock = false,
            "--no-decimate" => o.decimate = false,
            "--deblock" => {
                let v: String = parse("--deblock", args.next())?;
                let (a, b) = v.split_once(':').ok_or("--deblock expects A:B")?;
                o.deblock_offsets = (
                    a.parse().map_err(|_| "bad --deblock value")?,
                    b.parse().map_err(|_| "bad --deblock value")?,
                );
            }
            "--chroma-qp-offset" => o.chroma_qp_offset = parse("--chroma-qp-offset", args.next())?,
            "--recon" => o.recon = Some(parse::<String>("--recon", args.next())?.into()),
            "--framemd5" => o.framemd5 = Some(parse::<String>("--framemd5", args.next())?.into()),
            "--stats" => o.stats = Some(parse::<String>("--stats", args.next())?.into()),
            "--viz-frame" => o.viz_frame = Some(parse("--viz-frame", args.next())?),
            "--verbose" | "-v" => o.verbose = true,
            "--quiet" | "-q" => o.quiet = true,
            "--size" => {
                let v: String = parse("--size", args.next())?;
                let (w, h) = v.split_once('x').ok_or("--size expects WxH")?;
                o.size = (
                    w.parse().map_err(|_| "bad --size")?,
                    h.parse().map_err(|_| "bad --size")?,
                );
            }
            "--fps" => o.fps = parse("--fps", args.next())?,
            "--seed" => o.seed = parse("--seed", args.next())?,
            "--pattern" => {
                let v: String = parse("--pattern", args.next())?;
                o.pattern = match v.as_str() {
                    "moving" => Pattern::Moving,
                    "noise" => Pattern::Noise,
                    "still" => Pattern::Still,
                    "extremes" => Pattern::Extremes,
                    _ => return Err(format!("unknown pattern {v}")),
                };
            }
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option {s}")),
            _ => {
                if have_input {
                    return Err(format!("unexpected argument {a}"));
                }
                o.input = a.into();
                have_input = true;
            }
        }
    }
    if !have_input {
        return Err("missing file argument".into());
    }
    if o.qp > 51 {
        return Err("--qp must be within 0..51".into());
    }
    if o.subpel > 2 {
        return Err("--subpel must be 0, 1 or 2".into());
    }
    if !(1..=64).contains(&o.me_range) {
        return Err("--me-range must be within 1..64".into());
    }
    Ok(o)
}

fn is_mp4(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("mp4" | "m4v" | "mov")
    )
}

struct Summary {
    width: usize,
    height: usize,
    fps: f64,
    frames: Vec<FrameStats>,
    seconds: f64,
    level_idc: u8,
    viz: Option<(usize, Vec<MbRecord>)>,
    mb_w: usize,
    mb_h: usize,
}

impl Summary {
    fn total_bytes(&self) -> usize {
        self.frames.iter().map(|f| f.bytes).sum()
    }

    fn kbps(&self) -> f64 {
        if self.frames.is_empty() {
            return 0.0;
        }
        self.total_bytes() as f64 * 8.0 * self.fps / self.frames.len() as f64 / 1000.0
    }

    /// PSNR from the mean squared error over all frames (per plane).
    fn global_psnr(&self) -> [f64; 3] {
        std::array::from_fn(|i| {
            let sse: u64 = self.frames.iter().map(|f| f.sse[i]).sum();
            let n = if i == 0 {
                self.width * self.height
            } else {
                self.width * self.height / 4
            };
            psnr(sse, (n * self.frames.len().max(1)) as u64)
        })
    }
}

fn frame_line(s: &FrameStats) -> String {
    format!(
        "frame {:5} {} qp {:2} {:7} B  Y {:6.2} U {:6.2} V {:6.2} dB  I4 {:4} I16 {:4} P {:4} skip {:4}",
        s.index,
        if s.idr { "I" } else { "P" },
        s.qp,
        s.bytes,
        s.psnr[0],
        s.psnr[1],
        s.psnr[2],
        s.n_i4,
        s.n_i16,
        s.n_inter,
        s.n_skip
    )
}

fn run_encode(o: &Options) -> Result<Summary, String> {
    let file = File::open(&o.input).map_err(|e| format!("cannot open {}: {e}", o.input.display()))?;
    let mut reader =
        Y4mReader::new(BufReader::with_capacity(1 << 20, file)).map_err(|e| format!("{}: {e}", o.input.display()))?;
    let h = reader.header;
    let mut cfg = Config::new(h.width, h.height, h.fps_num, h.fps_den);
    cfg.rc = match o.bitrate {
        Some(kbps) => RcMode::Abr {
            bps: kbps as f64 * 1000.0,
        },
        None => RcMode::ConstQp {
            qp: o.qp,
            i_offset: o.i_qp_offset,
        },
    };
    cfg.keyint = o.keyint;
    cfg.me_range = o.me_range;
    cfg.subpel = o.subpel;
    cfg.partitions = o.partitions;
    cfg.sub8x8 = o.sub8x8;
    cfg.intra_in_p = o.intra_in_p;
    cfg.deblock = o.deblock;
    cfg.alpha_offset_div2 = o.deblock_offsets.0;
    cfg.beta_offset_div2 = o.deblock_offsets.1;
    cfg.chroma_qp_offset = o.chroma_qp_offset;
    cfg.decimate = o.decimate;
    let mut enc = Encoder::new(cfg)?;
    let (sps, pps) = enc.headers();
    let fps = h.fps_num as f64 / h.fps_den as f64;

    let create = |p: &Path| File::create(p).map_err(|e| format!("cannot create {}: {e}", p.display()));
    let mut annexb: Vec<BufWriter<File>> = Vec::new();
    let mut mp4: Vec<Mp4Writer<BufWriter<File>>> = Vec::new();
    for p in &o.outputs {
        if is_mp4(p) {
            mp4.push(
                Mp4Writer::new(
                    BufWriter::new(create(p)?),
                    &sps,
                    &pps,
                    h.width,
                    h.height,
                    h.fps_num,
                    h.fps_den,
                )
                .map_err(|e| e.to_string())?,
            );
        } else {
            annexb.push(BufWriter::new(create(p)?));
        }
    }
    let mut recon_y4m = match &o.recon {
        Some(p) => {
            let mut w = BufWriter::new(create(p)?);
            y4m::write_header(&mut w, &h).map_err(|e| e.to_string())?;
            Some(w)
        }
        None => None,
    };
    let mut recon_raw = match &o.recon_raw {
        Some(p) => Some(BufWriter::new(create(p)?)),
        None => None,
    };
    let mut md5_out = match &o.framemd5 {
        Some(p) => Some(BufWriter::new(create(p)?)),
        None => None,
    };

    let mut src = enc.new_input_frame();
    let mut stats = Vec::new();
    let mut viz = None;
    let tty = std::io::stderr().is_terminal();
    let start = Instant::now();
    let mut last_print = Instant::now();
    let io_err = |e: std::io::Error| format!("write failed: {e}");
    let mut buf = Vec::new();
    loop {
        if o.frames.is_some_and(|n| stats.len() >= n) {
            break;
        }
        if !reader
            .read_frame(&mut src)
            .map_err(|e| format!("{}: {e}", o.input.display()))?
        {
            break;
        }
        let EncodedFrame {
            nal,
            stats: fs,
            records,
        } = enc.encode(&src);
        buf.clear();
        if fs.idr {
            sps.write_annexb(&mut buf);
            pps.write_annexb(&mut buf);
        }
        nal.write_annexb(&mut buf);
        for w in &mut annexb {
            w.write_all(&buf).map_err(io_err)?;
        }
        for m in &mut mp4 {
            m.write_sample(&nal, fs.idr).map_err(io_err)?;
        }
        if recon_y4m.is_some() || recon_raw.is_some() || md5_out.is_some() {
            buf.clear();
            enc.recon().write_i420(h.width, h.height, &mut buf);
            if let Some(w) = &mut recon_y4m {
                w.write_all(b"FRAME\n").map_err(io_err)?;
                w.write_all(&buf).map_err(io_err)?;
            }
            if let Some(w) = &mut recon_raw {
                w.write_all(&buf).map_err(io_err)?;
            }
            if let Some(w) = &mut md5_out {
                writeln!(w, "{}", md5_hex(&buf)).map_err(io_err)?;
            }
        }
        if o.viz_frame == Some(fs.index) {
            viz = Some((fs.index, records));
        }
        if o.verbose {
            eprintln!("{}", frame_line(&fs));
        } else if !o.quiet && tty && last_print.elapsed().as_millis() > 200 {
            let n = stats.len() + 1;
            eprint!(
                "\rframe {n} {} qp {:2} Y {:.2} dB  {:.1} fps   ",
                if fs.idr { "I" } else { "P" },
                fs.qp,
                fs.psnr[0],
                n as f64 / start.elapsed().as_secs_f64()
            );
            last_print = Instant::now();
        }
        stats.push(fs);
    }
    if !o.quiet && !o.verbose && tty {
        eprint!("\r{:60}\r", "");
    }
    let seconds = start.elapsed().as_secs_f64();
    for mut w in annexb {
        w.flush().map_err(io_err)?;
    }
    for m in mp4 {
        m.finish().map_err(io_err)?;
    }
    for w in [recon_y4m.as_mut(), recon_raw.as_mut(), md5_out.as_mut()]
        .into_iter()
        .flatten()
    {
        w.flush().map_err(io_err)?;
    }
    if stats.is_empty() {
        return Err(format!("{}: no frames", o.input.display()));
    }
    Ok(Summary {
        width: h.width,
        height: h.height,
        fps,
        frames: stats,
        seconds,
        level_idc: enc.params.level_idc,
        viz,
        mb_w: enc.mb_w,
        mb_h: enc.mb_h,
    })
}

fn print_summary(s: &Summary) {
    let n = s.frames.len();
    let p = s.global_psnr();
    let mbs = (s.mb_w * s.mb_h * n) as f64;
    let sum = |f: fn(&FrameStats) -> u32| s.frames.iter().map(|x| f(x) as f64).sum::<f64>() * 100.0 / mbs;
    println!(
        "{n} frames {}x{} @ {:.2} fps, level {}.{}",
        s.width,
        s.height,
        s.fps,
        s.level_idc / 10,
        s.level_idc % 10
    );
    println!("bitrate   {:.1} kbit/s ({} bytes)", s.kbps(), s.total_bytes());
    println!("PSNR      Y {:.2}  U {:.2}  V {:.2} dB", p[0], p[1], p[2]);
    println!(
        "macroblocks  I4x4 {:.1}%  I16x16 {:.1}%  inter {:.1}%  skip {:.1}%",
        sum(|f| f.n_i4),
        sum(|f| f.n_i16),
        sum(|f| f.n_inter),
        sum(|f| f.n_skip)
    );
    println!("speed     {:.1} fps ({:.2} s)", n as f64 / s.seconds, s.seconds);
}

fn write_stats(path: &Path, o: &Options, s: &Summary) -> Result<(), String> {
    let mut j = String::new();
    let p = s.global_psnr();
    let _ =
        write!(
        j,
        "{{\n\"input\":\"{}\",\n\"width\":{},\"height\":{},\"fps\":{:.4},\"level_idc\":{},\n\"mb_w\":{},\"mb_h\":{},\n",
        o.input.file_name().map(|f| f.to_string_lossy().replace(['"', '\\'], "_")).unwrap_or_default(),
        s.width,
        s.height,
        s.fps,
        s.level_idc,
        s.mb_w,
        s.mb_h
    );
    let _ = write!(
        j,
        "\"rate_control\":\"{}\",\"kbps\":{:.3},\"psnr\":[{:.4},{:.4},{:.4}],\"encode_fps\":{:.2},\n\"frames\":[\n",
        match o.bitrate {
            Some(k) => format!("bitrate {k} kbit/s"),
            None => format!("qp {}", o.qp),
        },
        s.kbps(),
        p[0],
        p[1],
        p[2],
        s.frames.len() as f64 / s.seconds
    );
    for (i, f) in s.frames.iter().enumerate() {
        let _ = writeln!(
            j,
            "{{\"i\":{},\"type\":\"{}\",\"qp\":{},\"bytes\":{},\"psnr\":[{:.3},{:.3},{:.3}],\"i4\":{},\"i16\":{},\"inter\":{},\"skip\":{},\"pcm\":{},\"parts\":[{},{},{},{}],\"subs\":[{},{},{},{}]}}{}",
            f.index,
            if f.idr { "I" } else { "P" },
            f.qp,
            f.bytes,
            f.psnr[0],
            f.psnr[1],
            f.psnr[2],
            f.n_i4,
            f.n_i16,
            f.n_inter,
            f.n_skip,
            f.n_pcm,
            f.parts[0],
            f.parts[1],
            f.parts[2],
            f.parts[3],
            f.subs[0],
            f.subs[1],
            f.subs[2],
            f.subs[3],
            if i + 1 < s.frames.len() { "," } else { "" }
        );
    }
    j.push(']');
    if let Some((index, records)) = &s.viz {
        let _ = write!(j, ",\n\"viz\":{{\"frame\":{index},\"mbs\":[\n");
        for (i, r) in records.iter().enumerate() {
            let body = match &r.mode {
                MbMode::I4 { modes } => format!("\"t\":\"I4\",\"modes\":{modes:?}"),
                MbMode::I16 { mode } => format!("\"t\":\"I16\",\"mode\":{mode}"),
                MbMode::Skip { mv } => format!("\"t\":\"S\",\"mv\":{mv:?}"),
                MbMode::Pcm => "\"t\":\"PCM\"".to_string(),
                MbMode::Inter { part, sub, mvs } => {
                    format!("\"t\":\"P\",\"part\":{part},\"sub\":{sub:?},\"mv\":{mvs:?}")
                }
            };
            let _ = writeln!(
                j,
                "{{{body},\"qp\":{},\"cbp\":{},\"bits\":{}}}{}",
                r.qp,
                r.cbp,
                r.bits,
                if i + 1 < records.len() { "," } else { "" }
            );
        }
        j.push_str("]}");
    }
    j.push_str("\n}\n");
    std::fs::write(path, j).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn cmd_encode(o: &Options) -> Result<(), String> {
    if o.outputs.is_empty() {
        return Err("encode needs -o <output>".into());
    }
    let s = run_encode(o)?;
    if !o.quiet {
        print_summary(&s);
        for p in &o.outputs {
            println!("wrote     {}", p.display());
        }
    }
    if let Some(p) = &o.stats {
        write_stats(p, o, &s)?;
    }
    Ok(())
}

fn cmd_check(o: &mut Options) -> Result<(), String> {
    let ffmpeg = find_ffmpeg().ok_or("ffmpeg not found: install it or set $FFMPEG to its path")?;
    let dir = std::env::temp_dir().join(format!("squeeze264-check-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let h264 = dir.join("out.h264");
    let mp4 = dir.join("out.mp4");
    let raw = dir.join("recon.yuv");
    o.outputs = vec![h264.clone(), mp4.clone()];
    o.recon_raw = Some(raw.clone());
    let result = (|| -> Result<bool, String> {
        let s = run_encode(o)?;
        if !o.quiet {
            print_summary(&s);
        }
        if let Some(p) = &o.stats {
            write_stats(p, o, &s)?;
        }
        let n = s.frames.len();
        let mut runs = vec![
            (Decoder::FfmpegSoftware, &h264, "Annex B"),
            (Decoder::FfmpegSoftware, &mp4, "MP4"),
        ];
        if has_videotoolbox(&ffmpeg) {
            runs.push((Decoder::VideoToolbox, &h264, "Annex B"));
            runs.push((Decoder::VideoToolbox, &mp4, "MP4"));
        } else {
            println!("note: this ffmpeg has no VideoToolbox hwaccel; hardware decoder check skipped");
        }
        let mut all_ok = true;
        for (decoder, path, container) in runs {
            let expected = BufReader::new(File::open(&raw).map_err(|e| e.to_string())?);
            let r = check_decode(&ffmpeg, decoder, path, s.width, s.height, expected).map_err(|e| e.to_string())?;
            if r.hw_refused {
                println!(
                    "SKIP  {:42} {:8} the hardware decoder produced no frames for this {}x{} stream (it needs at least 64x64 and real hardware)",
                    decoder.name(),
                    container,
                    s.width,
                    s.height
                );
                continue;
            }
            let ok = r.bit_exact(n);
            all_ok &= ok;
            println!(
                "{}  {:42} {:8} {}/{} frames identical{}",
                if ok { "PASS" } else { "FAIL" },
                decoder.name(),
                container,
                r.decoded_frames - r.mismatches.len().min(r.decoded_frames),
                n,
                if r.stderr.trim().is_empty() {
                    ", decoder silent".to_string()
                } else {
                    format!(", decoder said: {}", r.stderr.trim())
                }
            );
            if let Some((f, plane, x, y, want, got)) = r.first_diff {
                println!("      first difference: frame {f} plane {plane} at ({x},{y}): encoder {want}, decoder {got}");
            }
        }
        Ok(all_ok)
    })();
    std::fs::remove_dir_all(&dir).ok();
    match result {
        Ok(true) => {
            println!("bit-exact: the decoders reproduce the encoder's reconstruction exactly");
            Ok(())
        }
        Ok(false) => Err("decoder output differs from the encoder's reconstruction".into()),
        Err(e) => Err(e),
    }
}

fn cmd_gen(o: &Options) -> Result<(), String> {
    let (w, h) = o.size;
    if w < 2 || h < 2 || w % 2 != 0 || h % 2 != 0 {
        return Err("--size needs even width and height".into());
    }
    let n = o.frames.unwrap_or(60);
    let file = File::create(&o.input).map_err(|e| format!("cannot create {}: {e}", o.input.display()))?;
    let mut out = BufWriter::new(file);
    let hdr = Y4mHeader {
        width: w,
        height: h,
        fps_num: o.fps,
        fps_den: 1,
    };
    y4m::write_header(&mut out, &hdr).map_err(|e| e.to_string())?;
    for i in 0..n {
        let f = synth_frame(w, h, i, n, o.pattern, o.seed);
        y4m::write_frame(&mut out, &f, w, h).map_err(|e| e.to_string())?;
    }
    out.flush().map_err(|e| e.to_string())?;
    if !o.quiet {
        println!("wrote {} ({n} frames {w}x{h})", o.input.display());
    }
    Ok(())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();
    if cmd.is_empty() || cmd == "-h" || cmd == "--help" || cmd == "help" {
        print!("{USAGE}");
        return;
    }
    if cmd == "--version" || cmd == "-V" {
        println!("squeeze264 {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let result = parse_args(args).and_then(|mut o| match cmd.as_str() {
        "encode" => cmd_encode(&o),
        "check" => cmd_check(&mut o),
        "gen" => cmd_gen(&o),
        other => Err(format!("unknown command {other}; try --help")),
    });
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
