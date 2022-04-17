use bytes::Bytes;
use core::marker::{PhantomData, Unpin};
use core::pin::Pin;
use core::task::{Context, Poll};
use futures_core::stream::Stream;
use futures_sink::Sink;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec as Ldc};

pub use ciborium;

#[must_use = "streams/sinks do nothing unless polled"]
#[derive(Debug)]
pub struct WrappedByteStream<T: Unpin, In, Out>(Framed<T, Ldc>, PhantomData<fn(In) -> Out>);

impl<T: Unpin, In, Out> WrappedByteStream<T, In, Out> {
    pub fn new(bst: T) -> Self
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        let x = Ldc::builder()
            .little_endian()
            // maximal frame length: 2GB (suitable to dump large store entries)
            .max_frame_length(2 * 1024 * 1024 * 1024)
            .new_codec();
        Self(Framed::new(bst, x), PhantomData)
    }

    fn inner(self: Pin<&mut Self>) -> Pin<&mut Framed<T, Ldc>> {
        Pin::new(&mut Pin::into_inner(self).0)
    }
}

impl<T, In, Out> Stream for WrappedByteStream<T, In, Out>
where
    T: AsyncRead + Unpin,
    Out: for<'a> serde::Deserialize<'a>,
{
    type Item = Result<Out, ciborium::de::Error<std::io::Error>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Self::inner(self).poll_next(cx).map(|item| {
            item.map(|item2| {
                let item3: Out = ciborium::de::from_reader(item2?.as_ref())?;
                Ok(item3)
            })
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<T, In, Out> Sink<In> for WrappedByteStream<T, In, Out>
where
    T: AsyncWrite + Unpin,
    In: serde::Serialize,
{
    type Error = ciborium::ser::Error<std::io::Error>;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Self::inner(self).poll_ready(cx).map_err(|e| e.into())
    }

    // use Item without borrowing, because we otherwise get lifetime conflicts...
    fn start_send(self: Pin<&mut Self>, item: In) -> Result<(), Self::Error> {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&item, &mut buf)?;
        Self::inner(self)
            .start_send(Bytes::from(buf))
            .map_err(|e| e.into())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Self::inner(self).poll_flush(cx).map_err(|e| e.into())
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Self::inner(self).poll_close(cx).map_err(|e| e.into())
    }
}

pub type WbsServerSide<T> = WrappedByteStream<T, crate::Response, crate::Request>;
pub type WbsClientSide<T> = WrappedByteStream<T, crate::Request, crate::Response>;
