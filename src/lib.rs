#![feature(io_slice_advance, slice_ptr_range)]

pub mod buf;
pub mod io;
pub mod mpsc;
pub mod runtime;

use std::io::{IoSlice, IoSliceMut, Result as IOResult};
use std::net::{SocketAddr, TcpListener};
use std::thread;

use crate::io::TcpStream;
use crate::mpsc::{channel, Receiver, Sender};
use crate::runtime::Executor;

fn acceptor_thread(sock: SocketAddr, chan: Sender<TcpStream>) -> IOResult<()> {
    let listener = TcpListener::bind(sock)?;

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        let _ = stream.set_nodelay(true);

        if let Err(_) = chan.send(TcpStream::from_std(stream)) {
            break;
        }

        println!("INFO - Accepted new connection!");
    }

    println!("INFO - Listener shutting down!");

    Ok(())
}

async fn worker_driver(mut stream: TcpStream) {
    let mut rbuf = Vec::with_capacity(1024);
    let mut wbuf = Vec::with_capacity(1024);

    'outer: loop {
        let _bytes = {
            let slice = unsafe {
                std::slice::from_raw_parts_mut(
                    rbuf.as_mut_ptr_range().end,
                    rbuf.capacity() - rbuf.len(),
                )
            };
            let mut rbuf_slice = [IoSliceMut::new(slice)];
            let bytes = match stream.read_vectored(&mut rbuf_slice).await {
                Ok(0) => break,
                Ok(bytes) => bytes,
                Err(_e) => {
                    // println!("INFO - Closing because of error: {}", _e);
                    break;
                }
            };

            unsafe {
                rbuf.set_len(rbuf.len() + bytes);
            }

            bytes
        };

        // println!("INFO - Read {} bytes", _bytes);

        let mut idx = 0;
        while rbuf.len() - idx >= 6 {
            if &rbuf[idx..idx + 6] != b"PING\r\n" {
                break 'outer;
            }

            idx += 6;
            wbuf.extend_from_slice(b"PONG\r\n");
        }

        let mut wslice = [IoSlice::new(&wbuf)];
        if let Err(e) = stream.write_all_vectored(&mut wslice).await {
            println!("INFO - Closing because of error: {}", e);
            break;
        };

        wbuf.clear();
        rbuf.drain(..idx);
    }
}

async fn worker(mut recv: Receiver<TcpStream>) {
    while let Ok(stream) = recv.recv().await {
        // println!("INFO - Starting new worker!");
        runtime::spawn(worker_driver(stream));
    }
}

pub fn real_main() -> IOResult<()> {
    let (send, recv) = channel()?;
    let sock = "0.0.0.0:12321".parse().unwrap();

    println!("INFO - Starting server on {}", sock);

    let thread = thread::spawn(move || acceptor_thread(sock, send));

    let mut exec = Executor::new();

    exec.spawn(worker(recv));

    exec.run();

    thread.join().unwrap()?;

    Ok(())
}
