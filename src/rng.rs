//! Small deterministic PRNG (splitmix64 seeding an xorshift64*), used by the
//! synthetic clip generator, the fuzzing mode and the tests. Not for security.

#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // splitmix64 step so that small seeds give well-mixed states.
        let mut z = seed.wrapping_add(0x9E3779B97F4A7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        Rng(if z == 0 { 0x2545F4914F6CDD1D } else { z })
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    /// Uniform in [0, n).
    #[inline]
    pub fn below(&mut self, n: u32) -> u32 {
        ((self.next_u64() >> 32) * n as u64 >> 32) as u32
    }

    /// Uniform in [lo, hi] inclusive.
    #[inline]
    pub fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1) as u32) as i32
    }

    /// True with probability num/den.
    #[inline]
    pub fn chance(&mut self, num: u32, den: u32) -> bool {
        self.below(den) < num
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_in_range() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        let mut seen = [false; 7];
        for _ in 0..1000 {
            let v = a.range(-3, 3);
            assert_eq!(v, b.range(-3, 3));
            assert!((-3..=3).contains(&v));
            seen[(v + 3) as usize] = true;
        }
        assert!(seen.iter().all(|&s| s));
        assert_ne!(Rng::new(1).next_u64(), Rng::new(2).next_u64());
    }
}
