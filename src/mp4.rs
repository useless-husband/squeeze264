//! Minimal ISO base media file (MP4) muxer for one AVC video track
//! (ISO/IEC 14496-12 and 14496-15). Samples go into `mdat` as they are
//! produced; the index (`moov`) is written at the end.

use crate::bitstream::Nal;
use std::io::{self, Seek, SeekFrom, Write};

pub struct Mp4Writer<W: Write + Seek> {
    w: W,
    sps: Vec<u8>,
    pps: Vec<u8>,
    width: u32,
    height: u32,
    timescale: u32,
    sample_delta: u32,
    mdat_start: u64,
    pos: u64,
    sizes: Vec<u32>,
    offsets: Vec<u64>,
    sync: Vec<u32>,
}

/// Builds a box: 32-bit size, four-character type, payload.
fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(payload.len() + 8);
    b.extend_from_slice(&(payload.len() as u32 + 8).to_be_bytes());
    b.extend_from_slice(kind);
    b.extend_from_slice(payload);
    b
}

/// Builds a "full box": version and flags precede the payload.
fn full_box(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(payload.len() + 4);
    p.extend_from_slice(&((version as u32) << 24 | flags).to_be_bytes());
    p.extend_from_slice(payload);
    boxed(kind, &p)
}

fn be32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn be16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}

const UNITY_MATRIX: [u32; 9] = [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000];

impl<W: Write + Seek> Mp4Writer<W> {
    pub fn new(
        mut w: W,
        sps: &Nal,
        pps: &Nal,
        width: usize,
        height: usize,
        fps_num: u32,
        fps_den: u32,
    ) -> io::Result<Self> {
        // A coarse timescale such as 25 is legal but some players round
        // badly with it, so scale small rates up.
        let (timescale, sample_delta) = if fps_num < 1000 {
            (fps_num * 1000, fps_den * 1000)
        } else {
            (fps_num, fps_den)
        };
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"isom");
        be32(&mut ftyp, 0x200);
        for brand in [b"isom", b"iso2", b"avc1", b"mp41"] {
            ftyp.extend_from_slice(brand);
        }
        let ftyp = boxed(b"ftyp", &ftyp);
        w.write_all(&ftyp)?;
        // mdat with a 64-bit size field, patched in finish().
        let mdat_start = ftyp.len() as u64;
        w.write_all(&1u32.to_be_bytes())?;
        w.write_all(b"mdat")?;
        w.write_all(&0u64.to_be_bytes())?;
        Ok(Mp4Writer {
            w,
            sps: sps.bytes.clone(),
            pps: pps.bytes.clone(),
            width: width as u32,
            height: height as u32,
            timescale,
            sample_delta,
            mdat_start,
            pos: mdat_start + 16,
            sizes: Vec::new(),
            offsets: Vec::new(),
            sync: Vec::new(),
        })
    }

    /// Appends one access unit consisting of a single slice NAL unit,
    /// stored with a 4-byte length prefix as avcC declares.
    pub fn write_sample(&mut self, nal: &Nal, sync: bool) -> io::Result<()> {
        let len = nal.bytes.len() as u32;
        self.w.write_all(&len.to_be_bytes())?;
        self.w.write_all(&nal.bytes)?;
        self.offsets.push(self.pos);
        self.sizes.push(len + 4);
        if sync {
            self.sync.push(self.sizes.len() as u32);
        }
        self.pos += len as u64 + 4;
        Ok(())
    }

    fn avcc(&self) -> Vec<u8> {
        let mut c = vec![
            1,           // configurationVersion
            self.sps[1], // AVCProfileIndication
            self.sps[2], // profile_compatibility
            self.sps[3], // AVCLevelIndication
            0xff,        // reserved + lengthSizeMinusOne = 3
            0xe1,        // reserved + numOfSequenceParameterSets = 1
        ];
        be16(&mut c, self.sps.len() as u16);
        c.extend_from_slice(&self.sps);
        c.push(1); // numOfPictureParameterSets
        be16(&mut c, self.pps.len() as u16);
        c.extend_from_slice(&self.pps);
        boxed(b"avcC", &c)
    }

    fn stbl(&self) -> Vec<u8> {
        let n = self.sizes.len() as u32;
        // Sample description: avc1 visual sample entry.
        let mut e = vec![0u8; 6]; // reserved
        be16(&mut e, 1); // data_reference_index
        e.extend_from_slice(&[0u8; 16]); // pre_defined / reserved
        be16(&mut e, self.width as u16);
        be16(&mut e, self.height as u16);
        be32(&mut e, 0x0048_0000); // 72 dpi
        be32(&mut e, 0x0048_0000);
        be32(&mut e, 0); // reserved
        be16(&mut e, 1); // frame_count
        let mut name = [0u8; 32];
        name[0] = 10;
        name[1..11].copy_from_slice(b"squeeze264");
        e.extend_from_slice(&name); // compressorname (Pascal string)
        be16(&mut e, 0x0018); // depth
        be16(&mut e, 0xffff); // pre_defined = -1
        e.extend_from_slice(&self.avcc());
        let avc1 = boxed(b"avc1", &e);
        let mut stsd = Vec::new();
        be32(&mut stsd, 1);
        stsd.extend_from_slice(&avc1);

        let mut stts = Vec::new();
        be32(&mut stts, 1);
        be32(&mut stts, n);
        be32(&mut stts, self.sample_delta);

        let mut stss = Vec::new();
        be32(&mut stss, self.sync.len() as u32);
        for &s in &self.sync {
            be32(&mut stss, s);
        }

        // One sample per chunk keeps the chunk table trivial.
        let mut stsc = Vec::new();
        be32(&mut stsc, 1);
        be32(&mut stsc, 1);
        be32(&mut stsc, 1);
        be32(&mut stsc, 1);

        let mut stsz = Vec::new();
        be32(&mut stsz, 0);
        be32(&mut stsz, n);
        for &s in &self.sizes {
            be32(&mut stsz, s);
        }

        let chunk_offsets = if self.offsets.last().is_some_and(|&o| o > u32::MAX as u64) {
            let mut co = Vec::new();
            be32(&mut co, n);
            for &o in &self.offsets {
                co.extend_from_slice(&o.to_be_bytes());
            }
            full_box(b"co64", 0, 0, &co)
        } else {
            let mut co = Vec::new();
            be32(&mut co, n);
            for &o in &self.offsets {
                be32(&mut co, o as u32);
            }
            full_box(b"stco", 0, 0, &co)
        };

        let mut stbl = full_box(b"stsd", 0, 0, &stsd);
        stbl.extend(full_box(b"stts", 0, 0, &stts));
        stbl.extend(full_box(b"stss", 0, 0, &stss));
        stbl.extend(full_box(b"stsc", 0, 0, &stsc));
        stbl.extend(full_box(b"stsz", 0, 0, &stsz));
        stbl.extend(chunk_offsets);
        boxed(b"stbl", &stbl)
    }

    /// Patches the mdat size and writes the movie index.
    pub fn finish(mut self) -> io::Result<W> {
        let duration = self.sizes.len() as u32 * self.sample_delta;

        let mut mvhd = Vec::new();
        be32(&mut mvhd, 0); // creation_time
        be32(&mut mvhd, 0); // modification_time
        be32(&mut mvhd, self.timescale);
        be32(&mut mvhd, duration);
        be32(&mut mvhd, 0x0001_0000); // rate 1.0
        be16(&mut mvhd, 0x0100); // volume
        mvhd.extend_from_slice(&[0u8; 10]);
        for m in UNITY_MATRIX {
            be32(&mut mvhd, m);
        }
        mvhd.extend_from_slice(&[0u8; 24]); // pre_defined
        be32(&mut mvhd, 2); // next_track_ID

        let mut tkhd = Vec::new();
        be32(&mut tkhd, 0);
        be32(&mut tkhd, 0);
        be32(&mut tkhd, 1); // track_ID
        be32(&mut tkhd, 0);
        be32(&mut tkhd, duration);
        tkhd.extend_from_slice(&[0u8; 8]);
        be16(&mut tkhd, 0); // layer
        be16(&mut tkhd, 0); // alternate_group
        be16(&mut tkhd, 0); // volume (video)
        be16(&mut tkhd, 0);
        for m in UNITY_MATRIX {
            be32(&mut tkhd, m);
        }
        be32(&mut tkhd, self.width << 16);
        be32(&mut tkhd, self.height << 16);

        let mut mdhd = Vec::new();
        be32(&mut mdhd, 0);
        be32(&mut mdhd, 0);
        be32(&mut mdhd, self.timescale);
        be32(&mut mdhd, duration);
        be16(&mut mdhd, 0x55c4); // language "und"
        be16(&mut mdhd, 0);

        let mut hdlr = Vec::new();
        be32(&mut hdlr, 0);
        hdlr.extend_from_slice(b"vide");
        hdlr.extend_from_slice(&[0u8; 12]);
        hdlr.extend_from_slice(b"VideoHandler\0");

        let mut vmhd = Vec::new();
        vmhd.extend_from_slice(&[0u8; 8]); // graphicsmode, opcolor

        let url = full_box(b"url ", 0, 1, &[]); // media is in this file
        let mut dref = Vec::new();
        be32(&mut dref, 1);
        dref.extend_from_slice(&url);
        let dinf = boxed(b"dinf", &full_box(b"dref", 0, 0, &dref));

        let mut minf = full_box(b"vmhd", 0, 1, &vmhd);
        minf.extend(dinf);
        minf.extend(self.stbl());

        let mut mdia = full_box(b"mdhd", 0, 0, &mdhd);
        mdia.extend(full_box(b"hdlr", 0, 0, &hdlr));
        mdia.extend(boxed(b"minf", &minf));

        // Track flags: enabled, in movie, in preview.
        let mut trak = full_box(b"tkhd", 0, 7, &tkhd);
        trak.extend(boxed(b"mdia", &mdia));

        let mut moov = full_box(b"mvhd", 0, 0, &mvhd);
        moov.extend(boxed(b"trak", &trak));
        let moov = boxed(b"moov", &moov);

        self.w.write_all(&moov)?;
        let mdat_size = self.pos - self.mdat_start;
        self.w.seek(SeekFrom::Start(self.mdat_start + 8))?;
        self.w.write_all(&mdat_size.to_be_bytes())?;
        self.w.seek(SeekFrom::End(0))?;
        self.w.flush()?;
        Ok(self.w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitstream::NalType;
    use std::io::Cursor;

    /// Walks sibling boxes in `data`, returning (type, payload range).
    fn boxes(data: &[u8], mut pos: usize, end: usize) -> Vec<(String, usize, usize)> {
        let mut out = Vec::new();
        while pos < end {
            let mut size = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            let kind = String::from_utf8_lossy(&data[pos + 4..pos + 8]).into_owned();
            let mut header = 8;
            if size == 1 {
                size = u64::from_be_bytes(data[pos + 8..pos + 16].try_into().unwrap()) as usize;
                header = 16;
            }
            assert!(size >= header && pos + size <= end, "box {kind} overruns its parent");
            out.push((kind, pos + header, pos + size));
            pos += size;
        }
        assert_eq!(pos, end);
        out
    }

    fn find(data: &[u8], path: &[&str]) -> (usize, usize) {
        let (mut s, mut e) = (0, data.len());
        for name in path {
            let b = boxes(data, s, e);
            let hit = b
                .iter()
                .find(|(k, _, _)| k == name)
                .unwrap_or_else(|| panic!("box {name} missing"));
            s = hit.1;
            e = hit.2;
        }
        (s, e)
    }

    fn u32_at(d: &[u8], p: usize) -> u32 {
        u32::from_be_bytes(d[p..p + 4].try_into().unwrap())
    }

    #[test]
    fn structure_and_sample_tables() {
        let sps = Nal::new(NalType::Sps, 3, &[66, 0xc0, 13, 0xaa, 0xbb]);
        let pps = Nal::new(NalType::Pps, 3, &[0xce, 0x38, 0x80]);
        let mut m = Mp4Writer::new(Cursor::new(Vec::new()), &sps, &pps, 352, 288, 30000, 1001).unwrap();
        let payloads: Vec<Vec<u8>> = (0..5).map(|i| vec![0x11 * (i as u8 + 1); 10 + 7 * i]).collect();
        for (i, p) in payloads.iter().enumerate() {
            let kind = if i % 3 == 0 { NalType::IdrSlice } else { NalType::Slice };
            m.write_sample(&Nal::new(kind, 2, p), i % 3 == 0).unwrap();
        }
        let data = m.finish().unwrap().into_inner();

        let top = boxes(&data, 0, data.len());
        let kinds: Vec<&str> = top.iter().map(|b| b.0.as_str()).collect();
        assert_eq!(kinds, ["ftyp", "mdat", "moov"]);

        let stbl = ["moov", "trak", "mdia", "minf", "stbl"];
        let (s, _) = find(&data, &[&stbl[..], &["stsz"]].concat());
        assert_eq!(u32_at(&data, s + 8), 5);
        let (o, _) = find(&data, &[&stbl[..], &["stco"]].concat());
        assert_eq!(u32_at(&data, o + 4), 5);
        for (i, p) in payloads.iter().enumerate() {
            let size = u32_at(&data, s + 12 + 4 * i) as usize;
            let off = u32_at(&data, o + 8 + 4 * i) as usize;
            assert_eq!(size, p.len() + 1 + 4);
            // Each sample is a length-prefixed NAL unit inside mdat.
            assert_eq!(u32_at(&data, off) as usize, p.len() + 1);
            assert_eq!(&data[off + 5..off + size], &p[..]);
            assert!(off >= top[1].1 && off + size <= top[1].2);
        }
        let (ss, _) = find(&data, &[&stbl[..], &["stss"]].concat());
        assert_eq!(
            (u32_at(&data, ss + 4), u32_at(&data, ss + 8), u32_at(&data, ss + 12)),
            (2, 1, 4)
        );
        let (tt, _) = find(&data, &[&stbl[..], &["stts"]].concat());
        assert_eq!((u32_at(&data, tt + 8), u32_at(&data, tt + 12)), (5, 1001));
        let (mv, _) = find(&data, &["moov", "mvhd"]);
        assert_eq!((u32_at(&data, mv + 12), u32_at(&data, mv + 16)), (30000, 5005));

        // avcC carries the parameter sets verbatim.
        let (sd, sd_end) = find(&data, &[&stbl[..], &["stsd"]].concat());
        let entry = &data[sd + 8..sd_end];
        assert_eq!(&entry[4..8], b"avc1");
        assert_eq!(u16::from_be_bytes([entry[32], entry[33]]), 352);
        assert_eq!(u16::from_be_bytes([entry[34], entry[35]]), 288);
        let avcc = &entry[86..];
        assert_eq!(&avcc[4..8], b"avcC");
        assert_eq!(&avcc[8..14], &[1, 66, 0xc0, 13, 0xff, 0xe1]);
        assert_eq!(&avcc[16..16 + sps.bytes.len()], &sps.bytes[..]);
    }

    #[test]
    fn low_frame_rates_get_a_finer_timescale() {
        let sps = Nal::new(NalType::Sps, 3, &[66, 0xc0, 13]);
        let pps = Nal::new(NalType::Pps, 3, &[0xce]);
        let mut m = Mp4Writer::new(Cursor::new(Vec::new()), &sps, &pps, 16, 16, 25, 1).unwrap();
        m.write_sample(&Nal::new(NalType::IdrSlice, 3, &[1, 2, 3]), true)
            .unwrap();
        let data = m.finish().unwrap().into_inner();
        let (mv, _) = find(&data, &["moov", "mvhd"]);
        assert_eq!((u32_at(&data, mv + 12), u32_at(&data, mv + 16)), (25000, 1000));
    }
}
