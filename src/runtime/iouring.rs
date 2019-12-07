use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::io::ErrorKind;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use iou::*;

use super::*;

struct NotifyInfo {
    taskid: TaskId,
    task: TaskNotify,
}

pub struct RtInfo {
    current: Option<TaskId>,
    sq: SubmissionQueue<'static>,
    spawn: Vec<Pin<Box<dyn Future<Output = ()>>>>,
}

pub struct Executor {
    nextid: usize,
    futures: HashMap<TaskId, Pin<Box<dyn Future<Output = ()>>>>,
    spawn: Vec<Pin<Box<dyn Future<Output = ()>>>>,
}

impl Executor {
    pub fn new() -> Self {
        Self {
            nextid: 0,
            futures: HashMap::default(),
            spawn: Vec::new(),
        }
    }

    pub fn spawn_dyn(&mut self, fut: Pin<Box<dyn Future<Output = ()>>>) {
        self.spawn.push(fut);
    }
    pub fn spawn<F>(&mut self, fut: F)
    where
        F: Future<Output = ()> + 'static,
    {
        self.spawn_dyn(Box::pin(fut))
    }

    fn next_id(&mut self) -> TaskId {
        self.nextid += 1;
        TaskId(self.nextid - 1)
    }

    fn poll_cqe(&mut self, rtinfo: &RefCell<RtInfo>, cqe: CompletionQueueEvent) {
        let waker = RawWaker::new(std::ptr::null(), &vtable::VTABLE);
        let waker = unsafe { Waker::from_raw(waker) };
        let mut ctx = Context::from_waker(&waker);

        let notify = unsafe { &mut *(cqe.user_data() as usize as *mut NotifyInfo) };

        notify.task.result = Some(cqe.result());
        rtinfo.borrow_mut().current = Some(notify.taskid);

        let poll = {
            let fut = self
                .futures
                .get_mut(&notify.taskid)
                .unwrap_or_else(|| panic!("Unknown task id: {:?}", notify.taskid));
            let fut = fut.as_mut();

            fut.poll(&mut ctx)
        };

        match poll {
            Poll::Pending => (),
            Poll::Ready(_) => {
                rtinfo.borrow_mut().current = None;
                self.futures.remove(&notify.taskid);
            }
        }

        assert!(notify.task.refcnt == 0);
        let _ = unsafe { Box::from_raw(notify as *mut _) };
    }
    fn poll_new(&mut self, rtinfo: &RefCell<RtInfo>) {
        let waker = RawWaker::new(std::ptr::null(), &vtable::VTABLE);
        let waker = unsafe { Waker::from_raw(waker) };
        let mut ctx = Context::from_waker(&waker);

        loop {
            let mut fut = {
                let mut rtinfo = rtinfo.borrow_mut();
                match rtinfo.spawn.pop() {
                    Some(fut) => fut,
                    None => break,
                }
            };

            let id = self.next_id();
            rtinfo.borrow_mut().current = Some(id);

            match fut.as_mut().poll(&mut ctx) {
                Poll::Pending => (),
                Poll::Ready(_) => {
                    rtinfo.borrow_mut().current = None;
                    continue;
                }
            }

            self.futures.insert(id, fut);
        }
    }

    fn _run(mut self, rtinfo: &RefCell<RtInfo>, mut cq: CompletionQueue, _: Registrar) {
        self.poll_new(rtinfo);

        while !self.futures.is_empty() {
            let spawn_is_empty = {
                let borrow = rtinfo.borrow_mut();
                borrow.spawn.is_empty()
            };

            if spawn_is_empty {
                let cqe = match cq.wait_for_cqe() {
                    Ok(cqe) => cqe,
                    Err(e) => {
                        if e.kind() == ErrorKind::Interrupted {
                            continue;
                        }

                        panic!("Failed to wait for CQE: {}", e)
                    }
                };
                self.poll_cqe(rtinfo, cqe);
            } else {
                self.poll_new(rtinfo);
            }
        }
    }

    pub fn run(mut self) {
        let mut uring =
            IoUring::new_with_flags(1024, SetupFlags::empty()).expect("Failed to set up uring");

        let (sq, cq, reg) = uring.queues();

        let rtinfo = RefCell::new(RtInfo {
            current: None,
            sq: unsafe { std::mem::transmute(sq) },
            spawn: std::mem::replace(&mut self.spawn, Vec::new()),
        });

        RUNTIME.with(|x| x.set(Some(&rtinfo as *const _)));
        let rtinfo = &rtinfo;

        let res = catch_unwind(AssertUnwindSafe(|| {
            self._run(rtinfo, cq, reg);
        }));

        RUNTIME.with(|x| x.set(None));

        if let Err(e) = res {
            resume_unwind(e);
        }
    }
}

impl RtInfo {
    pub fn current_task(&self) -> Option<TaskId> {
        self.current
    }

    /// # Safety
    /// You promise you won't touch user_data
    pub unsafe fn next_sqe(&mut self) -> Option<(SubmissionQueueEvent, *mut TaskNotify)> {
        let mut sqe = self.sq.next_sqe()?;
        let data = Box::leak(Box::new(NotifyInfo {
            taskid: self.current.unwrap(),
            task: TaskNotify {
                refcnt: 1,
                result: None,
            },
        }));

        sqe.set_user_data(data as *mut NotifyInfo as _);

        Some((sqe, &mut data.task as *mut TaskNotify))
    }

    pub fn submit(&mut self) -> Result<usize> {
        self.sq.submit()
    }

    pub fn spawn(&mut self, fut: Pin<Box<dyn Future<Output = ()>>>) {
        self.spawn.push(fut);
    }
}

mod vtable {
    use super::*;

    pub const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake, drop);

    unsafe fn clone(x: *const ()) -> RawWaker {
        RawWaker::new(x, &VTABLE)
    }
    unsafe fn wake(_: *const ()) {}
    unsafe fn drop(_: *const ()) {}
}
