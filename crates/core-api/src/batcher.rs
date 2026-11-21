use std::mem;

use crate::MAX_FRAME_BYTES;

/// Accumulates raw output bytes and cuts them into frames of at most
/// [`MAX_FRAME_BYTES`]. Bytes are never altered.
#[derive(Default)]
pub(crate) struct Batcher {
    buf: Vec<u8>,
}

impl Batcher {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Append `chunk`; returns every frame that reached `MAX_FRAME_BYTES`.
    /// Any remainder stays buffered until [`Batcher::take`].
    pub(crate) fn push(&mut self, mut chunk: &[u8]) -> Vec<Vec<u8>> {
        let mut full = Vec::new();
        while !chunk.is_empty() {
            let n = (MAX_FRAME_BYTES - self.buf.len()).min(chunk.len());
            self.buf.extend_from_slice(&chunk[..n]);
            chunk = &chunk[n..];
            if self.buf.len() == MAX_FRAME_BYTES {
                full.push(mem::take(&mut self.buf));
            }
        }
        full
    }

    /// Whether nothing is buffered.
    pub(crate) fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Take whatever is buffered, or `None` if empty.
    pub(crate) fn take(&mut self) -> Option<Vec<u8>> {
        (!self.buf.is_empty()).then(|| mem::take(&mut self.buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_chunks_stay_buffered_until_take() {
        let mut b = Batcher::new();
        for _ in 0..100 {
            assert!(b.push(&[1u8; 10]).is_empty());
        }
        assert_eq!(b.take().unwrap().len(), 1000);
        assert!(b.take().is_none());
    }

    #[test]
    fn large_chunk_splits_exactly_at_max() {
        let mut b = Batcher::new();
        let frames = b.push(&vec![9u8; 3 * MAX_FRAME_BYTES + 1234]);
        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|f| f.len() == MAX_FRAME_BYTES));
        assert_eq!(b.take().unwrap().len(), 1234);
    }

    #[test]
    fn partial_then_overflow_fills_frame_first() {
        let mut b = Batcher::new();
        assert!(b.push(&vec![1u8; MAX_FRAME_BYTES - 1]).is_empty());
        let frames = b.push(&[2u8; 3]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), MAX_FRAME_BYTES);
        assert_eq!(frames[0][MAX_FRAME_BYTES - 1], 2);
        assert_eq!(b.take().unwrap(), vec![2u8; 2]);
    }

    #[test]
    fn bytes_are_preserved() {
        let mut b = Batcher::new();
        let raw = [0xff, 0xfe, 0x1b, 0x5b, 0x00];
        assert!(b.push(&raw).is_empty());
        assert_eq!(b.take().unwrap(), raw);
    }
}
