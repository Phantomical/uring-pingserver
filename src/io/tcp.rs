use std::future::Future;
use std::io::{Error, IoSlice, IoSliceMut, Result as IOResult};
use std::marker::PhantomData;
use std::net::{Shutdown, TcpStream as StdTcpStream};
use std::os::unix::io::{AsRawFd, RawFd};
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::runtime::{current_runtime, TaskNotify};

pub struct TcpStream {
    stream: StdTcpStream,
}

impl TcpStream {
    pub fn from_std(stream: StdTcpStream) -> Self {
        Self { stream }
    }

    pub fn into_std(self) -> StdTcpStream {
        self.stream
    }

    pub fn read_vectored<'a>(
        &'a mut self,
        bufs: &'a mut [IoSliceMut<'a>],
    ) -> impl Future<Output = IOResult<usize>> + 'a {
        async move { TcpReadv::new(self, bufs)?.await }
    }

    pub fn write_vectored<'a>(
        &'a mut self,
        bufs: &'a mut [IoSlice<'a>],
    ) -> impl Future<Output = IOResult<usize>> + 'a {
        async move { TcpWritev::new(self, bufs)?.await }
    }

    pub fn write_all_vectored<'a>(
        &'a mut self,
        bufs: &'a mut [IoSlice<'a>],
    ) -> impl Future<Output = IOResult<()>> + 'a {
        let mut total_len: usize = bufs.iter().map(|x| x.len()).sum();

        async move {
            while total_len != 0 {
                let bytes = TcpWritev::new(self, bufs)?.await?;

                total_len -= bytes;
                IoSlice::advance(bufs, bytes);
            }

            Ok(())
        }
    }

    pub fn shutdown(&mut self, how: Shutdown) -> Result<(), Error> {
        self.stream.shutdown(how)
    }
}

impl AsRawFd for TcpStream {
    fn as_raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}

struct TcpReadv<'b> {
    notify: *mut TaskNotify,
    _marker: PhantomData<&'b ()>,
}

impl<'b> TcpReadv<'b> {
    fn new(stream: &mut TcpStream, bufs: &'b mut [IoSliceMut]) -> IOResult<Self> {
        let mut runtime = unsafe { &*current_runtime().unwrap() }.borrow_mut();
        let (mut sqe, notify) = unsafe { runtime.next_sqe().unwrap() };

        unsafe {
            sqe.prep_read_vectored(stream.as_raw_fd(), bufs, 0);
        }

        runtime.submit()?;

        Ok(Self {
            notify,
            _marker: PhantomData,
        })
    }
}

impl Future for TcpReadv<'_> {
    type Output = Result<usize, Error>;

    fn poll(mut self: Pin<&mut Self>, _: &mut Context) -> Poll<Self::Output> {
        assert!(!self.notify.is_null());

        unsafe {
            let notify = &mut *self.notify;

            if notify.result.is_some() {
                notify.refcnt -= 1;
                return Poll::Ready(notify.result.take().unwrap());
            }

            Poll::Pending
        }
    }
}

struct TcpWritev<'b> {
    notify: *mut TaskNotify,
    _marker: PhantomData<&'b ()>,
}

impl<'b> TcpWritev<'b> {
    fn new(stream: &mut TcpStream, bufs: &'b [IoSlice]) -> IOResult<Self> {
        let mut runtime = unsafe { &*current_runtime().unwrap() }.borrow_mut();
        let (mut sqe, notify) = unsafe { runtime.next_sqe().unwrap() };

        unsafe {
            sqe.prep_write_vectored(stream.as_raw_fd(), bufs, 0);
        }

        runtime.submit()?;

        Ok(Self {
            notify,
            _marker: PhantomData,
        })
    }
}

impl Future for TcpWritev<'_> {
    type Output = Result<usize, Error>;

    fn poll(mut self: Pin<&mut Self>, _: &mut Context) -> Poll<Self::Output> {
        assert!(!self.notify.is_null());

        unsafe {
            let notify = &mut *self.notify;

            if notify.result.is_some() {
                notify.refcnt -= 1;
                return Poll::Ready(notify.result.take().unwrap());
            }

            Poll::Pending
        }
    }
}
