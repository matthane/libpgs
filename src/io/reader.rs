use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};

const DEFAULT_BUF_SIZE: usize = 64 * 1024; // 64 KiB

/// A buffered reader with seek-aware skip optimization and position tracking.
///
/// Wraps `BufReader` and adds:
/// - Absolute position tracking (avoids `stream_position()` syscalls)
/// - Skip-within-buffer: if the skip distance falls within the buffered region,
///   the buffer cursor is advanced instead of issuing a seek syscall
/// - Configurable buffer size
pub struct SeekBufReader<R> {
    inner: BufReader<R>,
    /// Absolute byte position in the underlying stream.
    pos: u64,
    /// Total bytes read from the underlying stream (for I/O accounting).
    bytes_read: u64,
}

impl<R: Read + Seek> SeekBufReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner: BufReader::with_capacity(DEFAULT_BUF_SIZE, inner),
            pos: 0,
            bytes_read: 0,
        }
    }

    pub fn with_capacity(capacity: usize, inner: R) -> Self {
        Self {
            inner: BufReader::with_capacity(capacity, inner),
            pos: 0,
            bytes_read: 0,
        }
    }

    /// Current absolute position in the stream.
    #[inline]
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Total bytes actually read from the underlying stream.
    /// Useful for I/O accounting to measure how much data was transferred.
    #[inline]
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Skip forward `n` bytes. Uses buffer consumption when possible,
    /// falls back to seek for larger skips.
    pub fn skip(&mut self, n: u64) -> io::Result<()> {
        if n == 0 {
            return Ok(());
        }

        // Check how many bytes are available in the buffer.
        let buffered = self.inner.buffer().len() as u64;

        if n <= buffered {
            // Skip within the buffer — no syscall needed.
            self.inner.consume(n as usize);
            self.pos += n;
            Ok(())
        } else {
            // Consume whatever is buffered, then seek past the rest.
            let remaining = n - buffered;
            self.inner.consume(buffered as usize);
            self.pos += buffered;
            self.inner.seek_relative(remaining as i64)?;
            self.pos += remaining;
            Ok(())
        }
    }

    /// Seek to an absolute position in the stream.
    pub fn seek_to(&mut self, pos: u64) -> io::Result<()> {
        if pos == self.pos {
            return Ok(());
        }

        // If seeking forward within a reasonable range, try skip.
        if pos > self.pos {
            let delta = pos - self.pos;
            let buffered = self.inner.buffer().len() as u64;
            if delta <= buffered {
                self.inner.consume(delta as usize);
                self.pos = pos;
                return Ok(());
            }
        }

        // General seek.
        self.inner.seek(SeekFrom::Start(pos))?;
        self.pos = pos;
        Ok(())
    }

    /// Read a big-endian u16.
    #[inline]
    pub fn read_u16_be(&mut self) -> io::Result<u16> {
        let mut buf = [0u8; 2];
        self.read_exact(&mut buf)?;
        Ok(u16::from_be_bytes(buf))
    }

    /// Read a big-endian u32.
    #[inline]
    pub fn read_u32_be(&mut self) -> io::Result<u32> {
        let mut buf = [0u8; 4];
        self.read_exact(&mut buf)?;
        Ok(u32::from_be_bytes(buf))
    }

    /// Read a big-endian u64.
    #[inline]
    pub fn read_u64_be(&mut self) -> io::Result<u64> {
        let mut buf = [0u8; 8];
        self.read_exact(&mut buf)?;
        Ok(u64::from_be_bytes(buf))
    }

    /// Read an unsigned integer of `n` bytes (1-8) in big-endian order.
    #[inline]
    pub fn read_uint_be(&mut self, n: usize) -> io::Result<u64> {
        debug_assert!(n >= 1 && n <= 8);
        let mut buf = [0u8; 8];
        self.read_exact(&mut buf[8 - n..])?;
        Ok(u64::from_be_bytes(buf))
    }

    /// Read a UTF-8 string of exactly `len` bytes.
    pub fn read_string(&mut self, len: usize) -> io::Result<String> {
        let mut buf = vec![0u8; len];
        self.read_exact(&mut buf)?;
        String::from_utf8(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Read exactly `len` bytes into a new Vec.
    pub fn read_bytes(&mut self, len: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        self.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Read and discard `n` bytes without seeking.
    ///
    /// Unlike `skip()`, this actually reads through the bytes, keeping the
    /// underlying I/O sequential. Essential for NAS throughput where seeks
    /// kill read-ahead caching.
    pub fn drain(&mut self, n: u64) -> io::Result<()> {
        let mut remaining = n;

        // First consume whatever is in the buffer.
        let buffered = self.inner.buffer().len() as u64;
        if buffered > 0 {
            let consume = remaining.min(buffered);
            self.inner.consume(consume as usize);
            self.pos += consume;
            self.bytes_read += consume;
            remaining -= consume;
        }

        // Read through the rest in chunks using the BufReader's internal buffer.
        // We use fill_buf + consume to avoid allocating a separate buffer.
        while remaining > 0 {
            let buf = self.inner.fill_buf()?;
            if buf.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected EOF during drain",
                ));
            }
            let consume = (remaining as usize).min(buf.len());
            self.inner.consume(consume);
            self.pos += consume as u64;
            self.bytes_read += consume as u64;
            remaining -= consume as u64;
        }

        Ok(())
    }

    /// Get the file size by seeking to the end and back.
    pub fn file_size(&mut self) -> io::Result<u64> {
        let current = self.pos;
        let size = self.inner.seek(SeekFrom::End(0))?;
        self.inner.seek(SeekFrom::Start(current))?;
        // BufReader buffer is now invalidated, but position is restored.
        self.pos = current;
        Ok(size)
    }
}

impl<R: Read + Seek> Read for SeekBufReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.pos += n as u64;
        self.bytes_read += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> SeekBufReader<R> {
    /// Read exactly `buf.len()` bytes.
    pub fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_exact(buf)?;
        let n = buf.len() as u64;
        self.pos += n;
        self.bytes_read += n;
        Ok(())
    }

    /// Try to read exactly `buf.len()` bytes.
    /// Returns `Ok(true)` on success, `Ok(false)` on clean EOF (zero bytes available).
    /// Returns an error if EOF occurs after a partial read.
    pub fn try_read_exact(&mut self, buf: &mut [u8]) -> io::Result<bool> {
        if buf.is_empty() {
            return Ok(true);
        }
        let mut filled = 0;
        while filled < buf.len() {
            match self.inner.read(&mut buf[filled..])? {
                0 => {
                    if filled == 0 {
                        return Ok(false);
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "unexpected EOF in middle of read",
                    ));
                }
                n => {
                    filled += n;
                    self.pos += n as u64;
                    self.bytes_read += n as u64;
                }
            }
        }
        Ok(true)
    }
}
