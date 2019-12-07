mod iouring;

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::io::Result;
use std::pin::Pin;

pub use self::iouring::{Executor, RtInfo};

thread_local! {
    static RUNTIME: Cell<Option<*const RefCell<RtInfo>>> = Cell::new(None);
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct TaskId(usize);

pub struct TaskNotify {
    pub refcnt: usize,
    pub result: Option<Result<usize>>,
}

pub fn current_runtime() -> Option<*const RefCell<RtInfo>> {
    RUNTIME.with(|x| x.get())
}

pub fn spawn_dyn(fut: Pin<Box<dyn Future<Output = ()>>>) {
    let rt = current_runtime().expect("No runtime is currently active - cannot spawn future");

    assert!(!rt.is_null());

    unsafe {
        (*rt).borrow_mut().spawn(fut);
    }
}

pub fn spawn<F>(fut: F)
where
    F: Future<Output = ()> + 'static,
{
    spawn_dyn(Box::pin(fut));
}
