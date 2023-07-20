use std::{
    fmt, io,
    pin::Pin,
    task::{ready, Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::SapNi;

pin_project_lite::pin_project! {
pub struct SapNiStream<S> {
    #[pin]
    inner: S,

    read_buf: SapNi,
    write_buf: SapNi,
    write_buf_offset: usize,
}
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
            write_buf_offset: 0,
        }
    }
}

impl<S: fmt::Debug> fmt::Debug for SapNiStream<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SapNiStream")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl<S> AsyncRead for SapNiStream<S>
where
    S: AsyncRead,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut this = self.project();

        loop {
            // Any ready object?
            if let Some(data) = this.read_buf.get_data() {
                buf.put_slice(data);
                let new_buf = this.read_buf.buffer.split_off(data.len());
                this.read_buf.buffer = new_buf;
                return Poll::Ready(Ok(()));
            }

            // We need to read from underlying stream
            let mut buf2 = ReadBuf::uninit(this.read_buf.buffer.spare_capacity_mut());
            if let Err(e) = ready!(this.inner.as_mut().poll_read(cx, &mut buf2)) {
                return Poll::Ready(Err(e));
            }
            let read_bytes = buf2.initialized().len();
            unsafe {
                this.read_buf
                    .buffer
                    .set_len(this.read_buf.buffer.len() + read_bytes);
            }
            continue;
        }
    }
}

impl<S> AsyncWrite for SapNiStream<S>
where
    S: AsyncWrite,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let mut this = self.project();
        this.write_buf.append_data(buf);

        while *this.write_buf_offset < *this.write_buf.buffer.len() {
            let to_write = &this.write_buf.buffer[*this.write_buf_offset..];
            match ready!(this.inner.as_mut().poll_write(cx, to_write)) {
                Ok(n) => {
                    *this.write_buf_offset += n;
                    continue;
                }
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
        let new_buf = this.write_buf.split_off(*this.write_buf_offset)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.project().inner.poll_shutdown(cx)
    }
}
