use std::{fmt, io};

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use super::SapNi;

pub struct SapNiStream<S> {
    inner: S,

    read_buf: SapNi,
    write_buf: SapNi,
}

impl<S> SapNiStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    pub fn new(stream: S) -> Self {
        Self {
            inner: stream,
            read_buf: SapNi {
                buffer: Vec::with_capacity(1024),
            },
            write_buf: SapNi {
                buffer: Vec::with_capacity(1024),
            },
        }
    }

    pub async fn pipe<S2: AsyncRead + AsyncWrite + Unpin>(
        &mut self,
        stream: &mut S2,
    ) -> io::Result<()> {
        self.read_buf.buffer.clear();
        self.write_buf.buffer.clear();

        self.write_buf.set_data(&[][..]);
        loop {
            tokio::select! {
                Ok(n) = self.write_buf.read_from_raw_reader(stream) => {
                    if n == 0 {
                        break;
                    }
                    self.inner.write_all(&self.write_buf.buffer[..]).await?;
                    self.write_buf.buffer.clear();
                }
                Ok(data) = self.read_buf.read_from_ni_reader(&mut self.inner) => {
                    stream.write_all(data).await?;
                },
            }
        }

        Ok(())
    }
}

impl<S: fmt::Debug> fmt::Debug for SapNiStream<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SapNiStream")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}
