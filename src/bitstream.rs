//! Bit-level writer for RBSP payloads, Exp-Golomb codes (H.264 clause 9.1)
//! and NAL unit encapsulation with emulation prevention (clause 7.4.1).

/// Accumulates bits MSB-first into a byte vector.
#[derive(Default, Clone)]
pub struct BitWriter {
    buf: Vec<u8>,
    acc: u64,
    nbits: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            buf: Vec::with_capacity(bytes),
            acc: 0,
            nbits: 0,
        }
    }

    /// Writes the low `n` bits of `value` (n <= 32).
    #[inline]
    pub fn put(&mut self, n: u32, value: u32) {
        debug_assert!(n <= 32);
        debug_assert!(n == 32 || (value as u64) < (1u64 << n));
        self.acc = (self.acc << n) | value as u64;
        self.nbits += n;
        while self.nbits >= 8 {
            self.nbits -= 8;
            self.buf.push((self.acc >> self.nbits) as u8);
        }
    }

    #[inline]
    pub fn put1(&mut self, bit: bool) {
        self.put(1, bit as u32);
    }

    /// Unsigned Exp-Golomb, ue(v).
    #[inline]
    pub fn ue(&mut self, v: u32) {
        let x = v as u64 + 1;
        let len = 64 - x.leading_zeros(); // number of significant bits
                                          // len-1 zero bits, then x in len bits.
        if len > 1 {
            self.put_long(len - 1, 0);
        }
        self.put_long(len, x);
    }

    /// Signed Exp-Golomb, se(v): k>0 maps to 2k-1, k<=0 maps to -2k.
    #[inline]
    pub fn se(&mut self, v: i32) {
        let code = if v > 0 {
            (v as u32) * 2 - 1
        } else {
            (-(v as i64) as u32) * 2
        };
        self.ue(code);
    }

    /// Truncated Exp-Golomb, te(v), with the given inclusive range maximum.
    pub fn te(&mut self, max: u32, v: u32) {
        if max > 1 {
            self.ue(v);
        } else {
            self.put1(v == 0);
        }
    }

    fn put_long(&mut self, n: u32, value: u64) {
        if n > 32 {
            self.put(n - 32, (value >> 32) as u32);
            self.put(32, value as u32);
        } else {
            self.put(n, value as u32);
        }
    }

    /// Remembers the current position so a failed attempt can be undone.
    pub fn mark(&self) -> (usize, u64, u32) {
        (self.buf.len(), self.acc, self.nbits)
    }

    /// Discards everything written after `mark`.
    pub fn rewind(&mut self, mark: (usize, u64, u32)) {
        self.buf.truncate(mark.0);
        self.acc = mark.1;
        self.nbits = mark.2;
    }

    /// Writes zero bits up to the next byte boundary (pcm_alignment_zero_bit).
    pub fn align_zero(&mut self) {
        if self.nbits > 0 {
            let pad = 8 - self.nbits;
            self.put(pad, 0);
        }
    }

    /// Appends whole bytes; the writer must be byte aligned.
    pub fn put_bytes(&mut self, bytes: &[u8]) {
        debug_assert_eq!(self.nbits, 0);
        self.buf.extend_from_slice(bytes);
    }

    /// Number of bits written so far.
    #[inline]
    pub fn bit_len(&self) -> u64 {
        self.buf.len() as u64 * 8 + self.nbits as u64
    }

    /// rbsp_trailing_bits(): a stop bit followed by zero bits up to a byte boundary.
    pub fn rbsp_trailing(&mut self) {
        self.put(1, 1);
        if self.nbits > 0 {
            let pad = 8 - self.nbits;
            self.put(pad, 0);
        }
    }

    /// Returns the bytes written. Must be byte-aligned.
    pub fn into_bytes(self) -> Vec<u8> {
        assert_eq!(self.nbits, 0, "bit writer not byte aligned");
        self.buf
    }
}

/// Bit cost of ue(v) without writing it.
#[inline]
pub fn ue_len(v: u32) -> u32 {
    let x = v as u64 + 1;
    2 * (64 - x.leading_zeros()) - 1
}

/// Bit cost of se(v) without writing it.
#[inline]
pub fn se_len(v: i32) -> u32 {
    let code = if v > 0 {
        (v as u32) * 2 - 1
    } else {
        (-(v as i64) as u32) * 2
    };
    ue_len(code)
}

/// NAL unit types used by this encoder (Table 7-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NalType {
    Slice = 1,
    IdrSlice = 5,
    Sps = 7,
    Pps = 8,
}

/// One NAL unit: header byte plus escaped payload, without a start code.
#[derive(Clone, Debug)]
pub struct Nal {
    pub kind: NalType,
    pub ref_idc: u8,
    /// Header byte followed by the payload with emulation prevention bytes.
    pub bytes: Vec<u8>,
}

impl Nal {
    /// Wraps an RBSP into a NAL unit, inserting emulation_prevention_three_byte
    /// wherever the payload would otherwise contain 00 00 00/01/02/03.
    pub fn new(kind: NalType, ref_idc: u8, rbsp: &[u8]) -> Self {
        let mut bytes = Vec::with_capacity(rbsp.len() + rbsp.len() / 64 + 2);
        bytes.push((ref_idc << 5) | kind as u8);
        let mut zeros = 0;
        for &b in rbsp {
            if zeros >= 2 && b <= 3 {
                bytes.push(3);
                zeros = 0;
            }
            bytes.push(b);
            zeros = if b == 0 { zeros + 1 } else { 0 };
        }
        Nal { kind, ref_idc, bytes }
    }

    /// Appends the NAL in Annex B byte-stream format (4-byte start code).
    pub fn write_annexb(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&self.bytes);
    }
}

/// Bit reader used by tests and by the test-only CAVLC decoder.
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn bit(&mut self) -> u32 {
        let byte = self.data[self.pos >> 3];
        let b = (byte >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        b as u32
    }

    pub fn bits(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit();
        }
        v
    }

    pub fn ue(&mut self) -> u32 {
        let mut zeros = 0;
        while self.bit() == 0 {
            zeros += 1;
        }
        if zeros == 0 {
            return 0;
        }
        ((1u64 << zeros) - 1 + self.bits(zeros) as u64) as u32
    }

    pub fn se(&mut self) -> i32 {
        let k = self.ue();
        if k & 1 == 1 {
            k.div_ceil(2) as i32
        } else {
            -((k / 2) as i32)
        }
    }

    pub fn bit_pos(&self) -> usize {
        self.pos
    }

    pub fn bits_left(&self) -> usize {
        self.data.len() * 8 - self.pos
    }
}

/// Removes emulation prevention bytes from a NAL payload (used by tests).
pub fn unescape(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len());
    let mut zeros = 0;
    for &b in payload {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits_of(f: impl FnOnce(&mut BitWriter)) -> String {
        let mut w = BitWriter::new();
        f(&mut w);
        let n = w.bit_len() as usize;
        w.rbsp_trailing();
        let bytes = w.into_bytes();
        let mut s = String::new();
        for i in 0..n {
            s.push(if bytes[i / 8] >> (7 - i % 8) & 1 == 1 { '1' } else { '0' });
        }
        s
    }

    // Table 9-2 of the specification: bit strings for codeNum 0..8.
    #[test]
    fn ue_matches_spec_table() {
        let expect = [
            "1", "010", "011", "00100", "00101", "00110", "00111", "0001000", "0001001",
        ];
        for (v, e) in expect.iter().enumerate() {
            assert_eq!(&bits_of(|w| w.ue(v as u32)), e, "codeNum {v}");
            assert_eq!(ue_len(v as u32) as usize, e.len());
        }
    }

    // Table 9-3: codeNum k maps to (-1)^(k+1) * ceil(k/2).
    #[test]
    fn se_matches_spec_mapping() {
        let values = [0, 1, -1, 2, -2, 3, -3];
        for (code, &v) in values.iter().enumerate() {
            assert_eq!(bits_of(|w| w.se(v)), bits_of(|w| w.ue(code as u32)));
            assert_eq!(se_len(v), ue_len(code as u32));
        }
    }

    #[test]
    fn exp_golomb_round_trip() {
        let mut w = BitWriter::new();
        let us = [0u32, 1, 2, 7, 8, 255, 256, 65535, 1 << 20, u32::MAX - 1];
        let ss = [0i32, 1, -1, 17, -17, 32767, -32768, 1 << 20, -(1 << 20)];
        for &u in &us {
            w.ue(u);
        }
        for &s in &ss {
            w.se(s);
        }
        w.te(1, 0);
        w.te(1, 1);
        w.te(5, 4);
        w.rbsp_trailing();
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        for &u in &us {
            assert_eq!(r.ue(), u);
        }
        for &s in &ss {
            assert_eq!(r.se(), s);
        }
        assert_eq!(r.bit(), 1); // te with max 1 is an inverted bit
        assert_eq!(r.bit(), 0);
        assert_eq!(r.ue(), 4);
    }

    #[test]
    fn trailing_bits_align() {
        let mut w = BitWriter::new();
        w.put(3, 0b101);
        w.rbsp_trailing();
        assert_eq!(w.into_bytes(), vec![0b1011_0000]);
        let mut w = BitWriter::new();
        w.put(8, 0xAB);
        w.rbsp_trailing();
        assert_eq!(w.into_bytes(), vec![0xAB, 0x80]);
    }

    #[test]
    fn mark_and_rewind() {
        let mut w = BitWriter::new();
        w.put(11, 0x5a5);
        let m = w.mark();
        let len = w.bit_len();
        w.ue(12345);
        w.put(32, 0xdead_beef);
        w.rewind(m);
        assert_eq!(w.bit_len(), len);
        w.align_zero();
        assert_eq!(w.bit_len(), 16);
        w.put_bytes(&[1, 2, 3]);
        assert_eq!(w.into_bytes(), vec![0xb4, 0xa0, 1, 2, 3]);
    }

    #[test]
    fn emulation_prevention() {
        let rbsp = [0, 0, 0, 0, 0, 1, 0, 0, 2, 0, 0, 3, 0, 0, 4, 0, 0];
        let nal = Nal::new(NalType::Slice, 2, &rbsp);
        assert_eq!(nal.bytes[0], 0x41);
        let body = &nal.bytes[1..];
        // No 00 00 0x with x<=2 may remain in the escaped payload.
        for w in body.windows(3) {
            assert!(!(w[0] == 0 && w[1] == 0 && w[2] <= 2), "start code emulated");
        }
        assert_eq!(unescape(body), rbsp);
        assert_eq!(body, &[0, 0, 3, 0, 0, 3, 0, 1, 0, 0, 3, 2, 0, 0, 3, 3, 0, 0, 4, 0, 0]);
    }
}
