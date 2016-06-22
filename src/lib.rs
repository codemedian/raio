extern crate libc;

use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{channel, Sender, Receiver};
use std::time::Duration;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::thread;
use std::os::unix::io::IntoRawFd;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::FromRawFd;

pub trait AsyncTcpListener {
    fn accept_async<F, T: Executor>(&self, event_loop: &T, accept_cb: F) where F: Fn(&mut TcpListener) -> EventControl + Send + 'static;
}

pub trait AsyncTcpStream {
    fn read_async<F, T: Executor>(&self, event_loop: &T, read_cb: F) where F: Fn(&mut TcpStream) -> EventControl + Send + 'static;
}

pub enum EventControl {
    KEEP,
    DELETE
}

pub struct Future;

pub trait Executor : Drop {
    fn new(name: &str) -> Self;
    fn execute<T, F: Fn(T) + Send + 'static>(&self, callback: F, context: T);
    fn schedule<T, F: Fn(T) + Send + 'static>(&self, callback: F, context: T, delay: Duration) -> Future;

    fn shutdown(&mut self);
    //fn add_accept_event(&self, callback: F);
    //

    fn accept<F: Fn(&mut TcpListener) -> EventControl + Send>(&self, listener: TcpListener, callback: F);
    fn read<F: Fn(&mut TcpStream) -> EventControl + Send>(&self, stream: TcpStream, callback: F);
}

impl AsyncTcpListener for TcpListener {
    fn accept_async<F, T: Executor>(&self, event_loop: &T, accept_cb: F) where F: Fn(&mut TcpListener) -> EventControl + Send + 'static {
        self.set_nonblocking(true).unwrap();

        event_loop.accept(self.try_clone().unwrap(), accept_cb);
    }
}

impl AsyncTcpStream for TcpStream {
    fn read_async<F, T: Executor>(&self, event_loop: &T, read_cb: F) where F: Fn(&mut TcpStream) -> EventControl + Send + 'static {
        self.set_nonblocking(true).unwrap();

        event_loop.read(self.try_clone().unwrap(), read_cb);
    }
}
//
//

enum ThreadMessage {
    SHUTDOWN,
    ADD_ACCEPT_EVENT {
        fd: i32,
        callback: Box<Fn(&mut TcpListener) -> EventControl + Send>
    },
    ADD_READ_EVENT {
        fd: i32,
        callback: Box<Fn(&mut TcpStream) -> EventControl + Send>
    }
}

pub struct SingleThreadedExecutor {
    join_handle: Option<JoinHandle<()>>,
    sender: Sender<ThreadMessage>
}

impl Executor for SingleThreadedExecutor {
    fn new(name: &str) -> Self {

        let (tx, rx): (Sender<ThreadMessage>, Receiver<ThreadMessage>)= channel();
        SingleThreadedExecutor {
            sender: tx,
            join_handle: Some(thread::spawn( move || {
                executor_loop(rx);
            }))
        }
    }

    fn execute<T, F: Fn(T) + Send + 'static>(&self, callback: F, context: T) {
        unimplemented!()
    }

    fn schedule<T, F: Fn(T) + Send + 'static>(&self, callback: F, context: T, delay: Duration) -> Future {
        unimplemented!()
    }

    fn accept<F: Fn(&mut TcpListener) -> EventControl + Send + 'static>(&self, listener: TcpListener, callback: F) {
        self.sender.send(ThreadMessage::ADD_ACCEPT_EVENT {
            fd: listener.into_raw_fd(),
            callback: Box::new(callback)
        }).unwrap();
    }

    fn read<F: Fn(&mut TcpStream) -> EventControl + Send + 'static>(&self, stream: TcpStream, callback: F) {
        self.sender.send(ThreadMessage::ADD_READ_EVENT {
            fd: stream.into_raw_fd(),
            callback: Box::new(callback)
        }).unwrap();
    }

    fn shutdown(&mut self) {
        //match self.sender.send(ThreadMessage::SHUTDOWN) {
            //Ok(()) => {},
            //Err(e) => println!("Error occurred!!")
        //}

        if let Some(x) = self.join_handle.take() {
            x.join();
        }
    }
}

impl Drop for SingleThreadedExecutor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

enum CallbackType {
    ACCEPT(Box<Fn(&mut TcpListener) -> EventControl>),
    READ(Box<Fn(&mut TcpStream) -> EventControl>)
}

fn executor_loop(receiver: Receiver<ThreadMessage>) {
    let mut nev: libc::c_int = 0;
    let mut readevents: [Option<(CallbackType)>; 10] = [None, None, None, None, None, None, None, None, None, None]; //TODO: this has to be max num of file descriptors big - wasteful but faster
    let mut ev_list: [libc::kevent; 32] = [ libc::kevent { ident: 0, filter: 0, flags: 0, fflags: 0, data: 0, udata: std::ptr::null_mut() };32];
    let timeout = Box::into_raw(Box::new(libc::timespec { tv_sec: 0, tv_nsec: 100 })); //Don't want to timeout, should use a pipe to notify that thread about new events
    let kq = unsafe { libc::kqueue() as i32 };

    loop {
        match receiver.try_recv() { //This should be registered with  kevent too
            Ok(ThreadMessage::SHUTDOWN)        => break,
            Ok(ThreadMessage::ADD_ACCEPT_EVENT{ fd, callback }) => {
                println!("registering accept event for fd {}", fd);
                readevents[fd as usize] = Some((CallbackType::ACCEPT(callback)));

                let ev_set = libc::kevent {
                    ident: fd as libc::uintptr_t,
                    filter: libc::EVFILT_READ,
                    flags: libc::EV_ADD,
                    fflags: 0,
                    data: 0,
                    udata: std::ptr::null_mut()
                };

                unsafe {
                    libc::kevent(kq, &ev_set, 1, std::ptr::null_mut(), 0, std::ptr::null_mut());
                }
            }
            Ok(ThreadMessage::ADD_READ_EVENT{ fd, callback }) => {
                println!("added read event");
                readevents[fd as usize] = Some((CallbackType::READ(callback)));

                let ev_set = libc::kevent {
                    ident: fd as libc::uintptr_t,
                    filter: libc::EVFILT_READ,
                    flags: libc::EV_ADD,
                    fflags: 0,
                    data: 0,
                    udata: std::ptr::null_mut()
                };

                unsafe {
                    libc::kevent(kq, &ev_set, 1, std::ptr::null_mut(), 0, std::ptr::null_mut());
                }
            }
            Err(_) => {}
        }

        unsafe {
            nev = libc::kevent(kq, std::ptr::null(), 0, ev_list.as_mut_ptr(), 32, timeout );
        }

        match nev {
            -1   => println!("Error occured"),
            0   => {}, //println!("Fired without any events"),
            num => {
                //println!("got {} events", num);

                for _ in  0..num {
                    let fd = ev_list[0].ident as i32;
                    if readevents[fd as usize].is_some() {
                        let mut deleted = false;
                        if let Some(ref cb_type) = readevents[fd as usize] {
                            match cb_type {
                                &CallbackType::ACCEPT(ref cb)  => unsafe {
                                    println!("accepting from fd {}", fd);
                                    let mut listener = TcpListener::from_raw_fd(fd);
                                    match (cb)(&mut listener) {
                                        EventControl::DELETE => {
                                            let ev_set = libc::kevent {
                                                ident: fd as libc::uintptr_t,
                                                filter: libc::EVFILT_READ,
                                                flags: libc::EV_DELETE,
                                                fflags: 0,
                                                data: 0,
                                                udata: std::ptr::null_mut()
                                            };

                                            libc::kevent(kq, &ev_set, 1, std::ptr::null_mut(), 0, std::ptr::null_mut());
                                            deleted = true;
                                        },
                                        EventControl::KEEP => {
                                            listener.into_raw_fd();
                                        }
                                    }
                                },
                                &CallbackType::READ(ref cb) => unsafe {
                                    println!("reading from fd {}", fd);
                                    let mut stream = TcpStream::from_raw_fd(fd);
                                    match (cb)(&mut stream) {
                                        EventControl::DELETE => {
                                            let ev_set = libc::kevent {
                                                ident: fd as libc::uintptr_t,
                                                filter: libc::EVFILT_READ,
                                                flags: libc::EV_DELETE,
                                                fflags: 0,
                                                data: 0,
                                                udata: std::ptr::null_mut()
                                            };

                                            libc::kevent(kq, &ev_set, 1, std::ptr::null_mut(), 0, std::ptr::null_mut());
                                            deleted = true;
                                        },
                                        EventControl::KEEP  => {
                                            stream.into_raw_fd();
                                        }
                                    }
                                }
                            }
                        }
                        if deleted == true {
                            readevents[fd as usize].take();
                        }
                        //println!("event for fd {}", fd);
                    }
                }
                //println!("finished {} events", num);
            }
        }
    }

    println!("Shutting down thread");
}
