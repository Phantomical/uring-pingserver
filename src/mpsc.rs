use std::future::Future;
use std::io::{Error, ErrorKind};
use std::os::unix::io::RawFd;
use std::pin::Pin;
use std::sync::mpsc::{self, TryRecvError};
use std::task::{Context, Poll};

use iou::PollMask;

use crate::runtime::{current_runtime, TaskNotify};

#[derive(Debug)]
pub struct Sender<T> {
    send: mpsc::Sender<T>,
    fd: RawFd,
}

impl<T> Sender<T> {
    pub fn send(&self, val: T) -> Result<(), mpsc::SendError<T>> {
        self.send.send(val)?;

        unsafe {
            let one = 1u64;
            libc::write(
                self.fd,
                &one as *const u64 as *const _,
                std::mem::size_of_val(&one),
            );
        }

        Ok(())
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        unsafe {
            let one = 1u64;
            libc::write(
                self.fd,
                &one as *const u64 as *const _,
                std::mem::size_of_val(&one),
            );
        }
    }
}

#[derive(Debug)]
pub struct Receiver<T> {
    recv: mpsc::Receiver<T>,
    fd: RawFd,
}

impl<T> Receiver<T> {
    pub async fn recv(&mut self) -> Result<T, Error> {
        loop {
            match self.recv.try_recv() {
                Ok(val) => return Ok(val),
                Err(TryRecvError::Disconnected) => {
                    return Err(Error::new(ErrorKind::Other, Box::new(mpsc::RecvError)))
                }
                Err(TryRecvError::Empty) => (),
            }

            let cnt = WaitNotify::new(self.fd)?.await?;

            unsafe {
                let mut one = 1u64;
                libc::read(
                    self.fd,
                    &mut one as *mut u64 as *mut _,
                    std::mem::size_of_val(&one),
                );

                println!("DEBUG - Queue signaled, waking up: {}, read: {}", cnt, one);
            }
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

pub fn channel<T>() -> Result<(Sender<T>, Receiver<T>), Error> {
    let fd = unsafe {
        libc::eventfd(
            0,
            libc::EFD_SEMAPHORE | libc::EFD_CLOEXEC | libc::EFD_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(Error::last_os_error());
    }

    let (send, recv) = mpsc::channel();

    Ok((Sender { send, fd }, Receiver { recv, fd }))
}

struct WaitNotify {
    notify: *mut TaskNotify,
}

impl WaitNotify {
    fn new(fd: RawFd) -> Result<Self, Error> {
        let mut runtime = unsafe { &*current_runtime().unwrap() }.borrow_mut();
        let (mut sqe, notify) = unsafe { runtime.next_sqe().unwrap() };

        unsafe {
            sqe.prep_poll_add(fd, PollMask::IN);
        }

        runtime.submit()?;

        Ok(Self { notify })
    }
}

impl Future for WaitNotify {
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

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            send: self.send.clone(),
            fd: self.fd,
        }
    }
}
