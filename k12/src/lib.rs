//! Experimental pure Rust implementation of the KangarooTwelve
//! cryptographic hash algorithm, based on the reference implementation:
//!
//! <https://github.com/gvanas/KeccakCodePackage/blob/master/Standalone/kangaroo_twelve-reference/K12.py>
//!
//! Some optimisations copied from: <https://github.com/RustCrypto/hashes/tree/master/sha3/src>

// Based off this translation originally by Diggory Hardy:
// <https://github.com/dhardy/hash-bench/blob/master/src/k12.rs>

#![no_std]
#![doc(html_logo_url = "https://raw.githubusercontent.com/RustCrypto/meta/master/logo_small.png")]
#![deny(unsafe_code)]
#![warn(missing_docs, rust_2018_idioms)]

// TODO(tarcieri): eliminate alloc requirement
extern crate alloc;

#[macro_use]
mod lanes;

// TODO(tarcieri): eliminate usage of `Vec`
use alloc::vec::Vec;
use core::cmp::min;

/// The KangarooTwelve extendable-output function (XOF).
#[derive(Debug, Default)]
pub struct KangarooTwelve {
    /// Input to be processed
    // TODO(tarcieri): don't store input in a `Vec`
    buffer: Vec<u8>,
}

impl KangarooTwelve {
    /// Create a new [`KangarooTwelve`] instance
    pub fn new() -> Self {
        Self::default()
    }

    /// Input data into the hash function
    pub fn input(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Chained input into the hash function
    pub fn chain(mut self, bytes: &[u8]) -> Self {
        self.input(bytes);
        self
    }

    /// Get the resulting output of the function
    pub fn result(self, customization: impl AsRef<[u8]>, output_len: usize) -> Vec<u8> {
        let b = 8192;
        let c = 256;

        let mut slice = Vec::new(); // S
        slice.extend_from_slice(self.buffer.as_ref());
        slice.extend_from_slice(customization.as_ref());
        slice.extend_from_slice(&right_encode(customization.as_ref().len())[..]);

        // === Cut the input string into chunks of b bytes ===
        let n = (slice.len() + b - 1) / b;
        let mut slices = Vec::with_capacity(n); // Si
        for i in 0..n {
            let ub = min((i + 1) * b, slice.len());
            slices.push(&slice[i * b..ub]);
        }

        if n == 1 {
            // === Process the tree with only a final node ===
            f(slices[0], 0x07, output_len)
        } else {
            // === Process the tree with kangaroo hopping ===
            // TODO: in parallel
            let mut intermediate = Vec::with_capacity(n - 1); // CVi
            for i in 0..n - 1 {
                intermediate.push(f(slices[i + 1], 0x0B, c / 8));
            }

            let mut node_star = Vec::new();
            node_star.extend_from_slice(slices[0]);
            node_star.extend_from_slice(&[3, 0, 0, 0, 0, 0, 0, 0]);
            for i in 0..n - 1 {
                node_star.extend_from_slice(&intermediate[i][..]);
            }
            node_star.extend_from_slice(&right_encode(n - 1));
            node_star.extend_from_slice(b"\xFF\xFF");

            f(&node_star[..], 0x06, output_len)
        }
    }
}

fn f(input: &[u8], suffix: u8, mut output_len: usize) -> Vec<u8> {
    let mut state = [0u8; 200];
    let max_block_size = 1344 / 8; // r, also known as rate in bytes

    // === Absorb all the input blocks ===
    // We unroll first loop, which allows simple copy
    let mut block_size = min(input.len(), max_block_size);
    state[0..block_size].copy_from_slice(&input[0..block_size]);

    let mut offset = block_size;
    while offset < input.len() {
        keccak(&mut state);
        block_size = min(input.len() - offset, max_block_size);
        for i in 0..block_size {
            // TODO: is this sufficiently optimisable or better to convert to u64 first?
            state[i] ^= input[i + offset];
        }
        offset += block_size;
    }
    if block_size == max_block_size {
        // TODO: condition is nearly always false; tests pass without this.
        // Why is it here?
        keccak(&mut state);
        block_size = 0;
    }

    // === Do the padding and switch to the squeezing phase ===
    state[block_size] ^= suffix;
    if ((suffix & 0x80) != 0) && (block_size == (max_block_size - 1)) {
        // TODO: condition is almost always false — in fact tests pass without
        // this block! So why is it here?
        keccak(&mut state);
    }
    state[max_block_size - 1] ^= 0x80;
    keccak(&mut state);

    // === Squeeze out all the output blocks ===
    let mut output = Vec::with_capacity(output_len);
    while output_len > 0 {
        block_size = min(output_len, max_block_size);
        output.extend_from_slice(&state[0..block_size]);
        output_len -= block_size;
        if output_len > 0 {
            keccak(&mut state);
        }
    }
    output
}

#[allow(unsafe_code)]
fn read_u64(bytes: &[u8; 8]) -> u64 {
    unsafe { *(bytes as *const _ as *const u64) }.to_le()
}

#[allow(unsafe_code)]
fn write_u64(val: u64) -> [u8; 8] {
    unsafe { *(&val.to_le() as *const u64 as *const _) }
}

fn keccak(state: &mut [u8; 200]) {
    let mut lanes = [0u64; 25];
    let mut y;
    for x in 0..5 {
        FOR5!(y, 5, {
            lanes[x + y] = read_u64(array_ref!(state, 8 * (x + y), 8));
        });
    }
    lanes::keccak(&mut lanes);
    for x in 0..5 {
        FOR5!(y, 5, {
            let i = 8 * (x + y);
            state[i..i + 8].copy_from_slice(&write_u64(lanes[x + y]));
        });
    }
}

fn right_encode(mut x: usize) -> Vec<u8> {
    let mut slice = Vec::new();
    while x > 0 {
        slice.push((x % 256) as u8);
        x /= 256;
    }
    slice.reverse();
    let len = slice.len();
    slice.push(len as u8);
    slice
}

#[cfg(test)]
mod test {
    use super::*;
    use core::iter;

    fn read_bytes<T: AsRef<[u8]>>(s: T) -> Vec<u8> {
        fn b(c: u8) -> u8 {
            match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => unreachable!(),
            }
        }
        let s = s.as_ref();
        let mut i = 0;
        let mut v = Vec::new();
        while i < s.len() {
            if s[i] == b' ' || s[i] == b'\n' {
                i += 1;
                continue;
            }

            let n = b(s[i]) * 16 + b(s[i + 1]);
            v.push(n);
            i += 2;
        }
        v
    }

    #[test]
    fn empty() {
        // Source: reference paper
        assert_eq!(
            KangarooTwelve::new().chain(b"").result(b"", 32),
            read_bytes(
                "1a c2 d4 50 fc 3b 42 05 d1 9d a7 bf ca
                1b 37 51 3c 08 03 57 7a c7 16 7f 06 fe 2c e1 f0 ef 39 e5"
            )
        );

        assert_eq!(
            KangarooTwelve::new().chain(b"").result(b"", 64),
            read_bytes(
                "1a c2 d4 50 fc 3b 42 05 d1 9d a7 bf ca
                1b 37 51 3c 08 03 57 7a c7 16 7f 06 fe 2c e1 f0 ef 39 e5 42 69 c0 56 b8 c8 2e
                48 27 60 38 b6 d2 92 96 6c c0 7a 3d 46 45 27 2e 31 ff 38 50 81 39 eb 0a 71"
            )
        );

        assert_eq!(
            KangarooTwelve::new().chain(b"").result("", 10032)[10000..],
            read_bytes(
                "e8 dc 56 36 42 f7 22 8c 84
                68 4c 89 84 05 d3 a8 34 79 91 58 c0 79 b1 28 80 27 7a 1d 28 e2 ff 6d"
            )[..]
        );
    }

    #[test]
    fn pat_m() {
        let expected = [
            "2b da 92 45 0e 8b 14 7f 8a 7c b6 29 e7 84 a0 58 ef ca 7c f7
                d8 21 8e 02 d3 45 df aa 65 24 4a 1f",
            "6b f7 5f a2 23 91 98 db 47 72 e3 64 78 f8 e1 9b 0f 37 12 05
                f6 a9 a9 3a 27 3f 51 df 37 12 28 88",
            "0c 31 5e bc de db f6 14 26 de 7d cf 8f b7 25 d1 e7 46 75 d7
                f5 32 7a 50 67 f3 67 b1 08 ec b6 7c",
            "cb 55 2e 2e c7 7d 99 10 70 1d 57 8b 45 7d df 77 2c 12 e3 22
                e4 ee 7f e4 17 f9 2c 75 8f 0d 59 d0",
            "87 01 04 5e 22 20 53 45 ff 4d da 05 55 5c bb 5c 3a f1 a7 71
                c2 b8 9b ae f3 7d b4 3d 99 98 b9 fe",
            "84 4d 61 09 33 b1 b9 96 3c bd eb 5a e3 b6 b0 5c c7 cb d6 7c
                ee df 88 3e b6 78 a0 a8 e0 37 16 82",
            "3c 39 07 82 a8 a4 e8 9f a6 36 7f 72 fe aa f1 32 55 c8 d9 58
                78 48 1d 3c d8 ce 85 f5 8e 88 0a f8",
        ];
        for i in 0..5
        /*NOTE: can be up to 7 but is slow*/
        {
            let len = 17usize.pow(i);
            let m: Vec<u8> = (0..len).map(|j| (j % 251) as u8).collect();
            let result = KangarooTwelve::new().chain(&m).result("", 32);
            assert_eq!(result, read_bytes(expected[i as usize]));
        }
    }

    #[test]
    fn pat_c() {
        let expected = [
            "fa b6 58 db 63 e9 4a 24 61 88 bf 7a f6 9a 13 30 45 f4 6e e9
                84 c5 6e 3c 33 28 ca af 1a a1 a5 83",
            "d8 48 c5 06 8c ed 73 6f 44 62 15 9b 98 67 fd 4c 20 b8 08 ac
                c3 d5 bc 48 e0 b0 6b a0 a3 76 2e c4",
            "c3 89 e5 00 9a e5 71 20 85 4c 2e 8c 64 67 0a c0 13 58 cf 4c
                1b af 89 44 7a 72 42 34 dc 7c ed 74",
            "75 d2 f8 6a 2e 64 45 66 72 6b 4f bc fc 56 57 b9 db cf 07 0c
                7b 0d ca 06 45 0a b2 91 d7 44 3b cf",
        ];
        for i in 0..4 {
            let m: Vec<u8> = iter::repeat(0xFF).take(2usize.pow(i) - 1).collect();
            let len = 41usize.pow(i);
            let c: Vec<u8> = (0..len).map(|j| (j % 251) as u8).collect();
            let result = KangarooTwelve::new().chain(&m).result(c, 32);
            assert_eq!(result, read_bytes(expected[i as usize]));
        }
    }
}
