//! Streaming YUV4MPEG2 reader and writer (4:2:0, 8-bit only).

use crate::frame::Frame;
use std::io::{self, BufRead, Read, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Y4mHeader {
    pub width: usize,
    pub height: usize,
    pub fps_num: u32,
    pub fps_den: u32,
}

pub struct Y4mReader<R: BufRead> {
    r: R,
    pub header: Y4mHeader,
    line: Vec<u8>,
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

impl<R: BufRead> Y4mReader<R> {
    pub fn new(mut r: R) -> io::Result<Self> {
        let mut line = Vec::new();
        r.by_ref().take(1024).read_until(b'\n', &mut line)?;
        if line.last() != Some(&b'\n') {
            return Err(bad("not a Y4M file: header line missing"));
        }
        line.pop();
        let text = String::from_utf8_lossy(&line).into_owned();
        let mut it = text.split(' ');
        if it.next() != Some("YUV4MPEG2") {
            return Err(bad("not a Y4M file: missing YUV4MPEG2 signature"));
        }
        let (mut width, mut height, mut fps) = (0usize, 0usize, (25u32, 1u32));
        for tok in it {
            if tok.is_empty() {
                continue;
            }
            let (tag, val) = tok.split_at(1);
            match tag {
                "W" => width = val.parse().map_err(|_| bad("bad width"))?,
                "H" => height = val.parse().map_err(|_| bad("bad height"))?,
                "F" => {
                    let (n, d) = val.split_once(':').ok_or_else(|| bad("bad frame rate"))?;
                    fps = (
                        n.parse().map_err(|_| bad("bad frame rate"))?,
                        d.parse().map_err(|_| bad("bad frame rate"))?,
                    );
                }
                "C" => {
                    // 4:2:0 variants differ only in chroma siting.
                    if !matches!(val, "420" | "420jpeg" | "420mpeg2" | "420paldv") {
                        return Err(bad(format!(
                            "unsupported chroma format C{val}: only 8-bit 4:2:0 is supported"
                        )));
                    }
                }
                "I" if !matches!(val, "p" | "?") => {
                    return Err(bad("interlaced input is not supported"));
                }
                _ => {}
            }
        }
        if width == 0 || height == 0 || fps.0 == 0 || fps.1 == 0 {
            return Err(bad("Y4M header lacks width, height or frame rate"));
        }
        if width > 16384 || height > 16384 {
            return Err(bad("picture too large"));
        }
        Ok(Y4mReader {
            r,
            header: Y4mHeader {
                width,
                height,
                fps_num: fps.0,
                fps_den: fps.1,
            },
            line: Vec::new(),
        })
    }

    /// Reads the next picture into `frame` (macroblock-aligned, no border)
    /// and replicates edges into the alignment padding.
    /// Returns Ok(false) at a clean end of stream.
    pub fn read_frame(&mut self, frame: &mut Frame) -> io::Result<bool> {
        self.line.clear();
        let n = self.r.by_ref().take(256).read_until(b'\n', &mut self.line)?;
        if n == 0 {
            return Ok(false);
        }
        if !self.line.starts_with(b"FRAME") || self.line.last() != Some(&b'\n') {
            return Err(bad("Y4M frame marker missing"));
        }
        let (w, h) = (self.header.width, self.header.height);
        for (i, p) in frame.planes.iter_mut().enumerate() {
            let (pw, ph) = if i == 0 { (w, h) } else { (w.div_ceil(2), h.div_ceil(2)) };
            for y in 0..ph {
                self.r.read_exact(&mut p.row_mut(y)[..pw])?;
            }
        }
        frame.pad_from_visible(w, h);
        Ok(true)
    }
}

/// Writes the Y4M stream header.
pub fn write_header<W: Write>(w: &mut W, h: &Y4mHeader) -> io::Result<()> {
    writeln!(
        w,
        "YUV4MPEG2 W{} H{} F{}:{} Ip A1:1 C420jpeg",
        h.width, h.height, h.fps_num, h.fps_den
    )
}

/// Writes one picture (visible area only).
pub fn write_frame<W: Write>(w: &mut W, frame: &Frame, vw: usize, vh: usize) -> io::Result<()> {
    w.write_all(b"FRAME\n")?;
    let mut buf = Vec::with_capacity(vw * vh * 3 / 2);
    frame.write_i420(vw, vh, &mut buf);
    w.write_all(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trip_with_odd_size() {
        let hdr = Y4mHeader {
            width: 18,
            height: 10,
            fps_num: 30000,
            fps_den: 1001,
        };
        let mut f = Frame::new(32, 16, 0);
        for (i, p) in f.planes.iter_mut().enumerate() {
            for (j, v) in p.data.iter_mut().enumerate() {
                *v = (j * 7 + i * 31) as u8;
            }
        }
        f.pad_from_visible(18, 10);
        let mut bytes = Vec::new();
        write_header(&mut bytes, &hdr).unwrap();
        write_frame(&mut bytes, &f, 18, 10).unwrap();
        write_frame(&mut bytes, &f, 18, 10).unwrap();

        let mut r = Y4mReader::new(Cursor::new(&bytes)).unwrap();
        assert_eq!(r.header, hdr);
        let mut g = Frame::new(32, 16, 0);
        assert!(r.read_frame(&mut g).unwrap());
        for i in 0..3 {
            assert_eq!(f.planes[i].data, g.planes[i].data);
        }
        assert!(r.read_frame(&mut g).unwrap());
        assert!(!r.read_frame(&mut g).unwrap());
    }

    #[test]
    fn rejects_unsupported_input() {
        let e = |s: &str| Y4mReader::new(Cursor::new(s.as_bytes().to_vec())).err();
        assert!(e("YUV4MPEG2 W16 H16 F25:1 C444\n").is_some());
        assert!(e("YUV4MPEG2 W16 H16 F25:1 C420p10\n").is_some());
        assert!(e("YUV4MPEG2 W16 H16 F25:1 It\n").is_some());
        assert!(e("RIFF....\n").is_some());
        assert!(e("YUV4MPEG2 W16 F25:1\n").is_some());
        assert!(e("YUV4MPEG2 W16 H16 F25:1 Ip A1:1 C420mpeg2 XYSCSS=420MPEG2\n").is_none());
    }

    #[test]
    fn truncated_frame_is_an_error() {
        let mut bytes = b"YUV4MPEG2 W16 H16 F25:1\nFRAME\n".to_vec();
        bytes.extend_from_slice(&[0u8; 100]);
        let mut r = Y4mReader::new(Cursor::new(bytes)).unwrap();
        let mut f = Frame::new(16, 16, 0);
        assert!(r.read_frame(&mut f).is_err());
    }
}
