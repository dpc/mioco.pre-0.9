
mod mioco {
    pub use super::super::*;
}

use std;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};

use time::{SteadyTime, Duration};

use std::thread;
use std::net::SocketAddr;
use net2::TcpBuilder;

#[cfg(windows)]
struct FakePipeReader(mioco::sync::mpsc::Receiver<u8>);
#[cfg(windows)]
struct FakePipeWriter(mioco::sync::mpsc::Sender<u8>);

#[cfg(windows)]
impl Read for FakePipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut i = 0;
        loop {
            if i >= buf.len() {
                return Ok(i);
            }
            if let Ok(byte) = if i == 0 {
                self.0.recv()
            } else {
                // no matter if disconnected or empty: we will just
                // return Ok(i)
                self.0.try_recv().map_err(|_| std::sync::mpsc::RecvError)
            } {
                buf[i] = byte;
                i += 1;
            } else {
                return Ok(i);
            }
        }
    }
}

#[cfg(windows)]
impl ::evented::EventedImpl for FakePipeReader {
    type Raw = <mioco::sync::mpsc::Receiver<u8> as ::evented::EventedImpl>::Raw;

    fn shared(&self) -> &::evented::RcEventSource<<mioco::sync::mpsc::Receiver<u8> as ::evented::EventedImpl>::Raw> {
        self.0.shared()
    }
}

#[cfg(windows)]
impl Write for FakePipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len();
        for i in 0..len {
            let _ = self.0.send(buf[i].clone());
        }
        Ok(len)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(not(windows))]
fn pipe() -> (mioco::unix::PipeReader, mioco::unix::PipeWriter) {
    mioco::unix::pipe().unwrap()
}

#[cfg(windows)]
fn pipe() -> (FakePipeReader, FakePipeWriter) {
    let (tx, rx) = mioco::sync::mpsc::channel();
    (FakePipeReader(rx), FakePipeWriter(tx))
}


const THREADS_N: [usize; 4] = [1, 2, 5, 21];

#[test]
fn empty_handler() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        mioco::start_threads(threads, move || {
            let mut lock = finished_copy.lock().unwrap();
            *lock = true;
        })
            .unwrap();

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn empty_subcoroutines() {
    for &threads in THREADS_N.iter() {
        let counter = Arc::new(Mutex::new(0i32));

        let counter_copy = counter.clone();

        mioco::start_threads(threads, move || {

            for _ in 0..512 {
                let counter_subcopy = counter_copy.clone();
                mioco::spawn(move || {
                    let mut lock = counter_subcopy.lock().unwrap();
                    *lock += 1;
                });
            }

            let mut lock = counter_copy.lock().unwrap();
            *lock += 1;
        })
            .unwrap();

        assert_eq!(*counter.lock().unwrap(), 512 + 1);
    }
}

fn silent_panic() -> ! {
    std::panic::propagate(Box::new("explicit panic silented by mioco/src/test.rs"));
}

#[test]
fn contain_panics() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let res = mioco::start_threads(threads, move || silent_panic());

        assert!(res.is_err());

        assert!(!*finished_ok.lock().unwrap());
    }
}

#[test]
fn contain_panics_in_subcoroutines() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        mioco::start_threads(threads, move || {

            for _ in 0..512 {
                mioco::spawn(|| silent_panic());
            }

            let mut lock = finished_copy.lock().unwrap();
            *lock = true;
        })
            .unwrap();

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
#[should_panic]
#[cfg(debug_assertions)] //optimizations seem to let this test fail. lets disable that for now.
fn propagate_uncatched_panic() {
    use {Mioco, Config};

    let _: std::thread::Result<()> = Mioco::new_configured({
                                         let mut config = Config::new();
                                         config.set_catch_panics(false);
                                         config.set_thread_num(1);
                                         config
                                     })
                                         .start(|| silent_panic());
}

#[test]
fn long_chain() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        mioco::start_threads(threads, move || {

            let (first_reader, first_writer) = pipe();

            let mut prev_reader = first_reader;

            for _ in 0..1024 {
                let (reader, writer) = pipe();

                mioco::spawn(move || {
                    let mut reader = prev_reader;
                    let mut writer = writer;

                    let _ = std::io::copy(&mut reader, &mut writer);
                });

                prev_reader = reader;
            }

            let mut first_writer = first_writer;
            let mut last_reader = prev_reader;

            for i in 0..9 {
                let test_str = format!("TeSt{}", i);
                let _ = first_writer.write_all(test_str.as_str().as_bytes());
                let mut buf = [0u8; 16];

                let _ = last_reader.read(&mut buf);

                if &buf[0..5] != test_str.as_str().as_bytes() {
                    silent_panic();
                }
            }

            let mut lock = finished_copy.lock().unwrap();
            *lock = true;
        })
            .unwrap();

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn lots_of_event_sources() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        mioco::start_threads(threads, move || {

            let (first_reader, first_writer) = pipe();

            let mut prev_reader = first_reader;

            for _ in 0..8 {
                let (reader, writer) = pipe();

                mioco::spawn(move || {
                    // This fake readers are not really used, they are just registered for the sake of
                    // testing if event sources registered with high id number are handled correctly
                    let mut readers = Vec::new();
                    let mut writers = Vec::new();
                    for _ in 0..100 {
                        let (r, w) = pipe();
                        readers.push(r);
                        writers.push(w);
                    }

                    let mut reader = prev_reader;
                    let mut writer = writer;

                    let _ = std::io::copy(&mut reader, &mut writer);
                });

                prev_reader = reader;
            }

            let mut first_writer = first_writer;
            let mut last_reader = prev_reader;

            for i in 0..9 {
                let test_str = format!("TeSt{}", i);
                let _ = first_writer.write_all(test_str.as_str().as_bytes());
                let mut buf = [0u8; 16];

                let _ = last_reader.read(&mut buf);

                if &buf[0..5] != test_str.as_str().as_bytes() {
                    silent_panic();
                }
            }

            let mut lock = finished_copy.lock().unwrap();
            *lock = true;
        }).unwrap();

        assert!(*finished_ok.lock().unwrap());
    }
}

/// Test if drop is performed on IOs when coroutine panics.
#[test]
fn destructs_io_on_panic() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_ok_copy = finished_ok.clone();
        mioco::start_threads(threads, move || {

            let (reader, writer) = pipe();

            mioco::spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; 16];
                let ret = reader.read(&mut buf);
                assert!(ret.is_ok());

                let mut lock = finished_ok_copy.lock().unwrap();
                *lock = true;
            });

            mioco::spawn(move || {
                let _writer = writer;
                silent_panic();
            });
        })
            .unwrap();

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn channel_disconnect_on_sender_drop() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_ok_copy = finished_ok.clone();
        mioco::start_threads(threads, move || {

            let (sender, receiver) = mioco::sync::mpsc::channel();

            mioco::spawn(move || {
                assert!(receiver.recv().is_ok());
                assert!(receiver.recv().is_err());
                let mut lock = finished_ok_copy.lock().unwrap();
                *lock = true;
            });

            mioco::spawn(move || {
                sender.send(0usize).unwrap();
                mioco::sleep_ms(10);

                silent_panic();
            });
        })
            .unwrap();

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn channel_disconnect_on_sender_drop_many() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_ok_copy = finished_ok.clone();
        mioco::start_threads(threads, move || {

            let (sender, receiver) = mioco::sync::mpsc::channel();
            const HOW_MANY: usize = 10;

            mioco::spawn(move || {
                for _ in 0..HOW_MANY {
                    assert!(receiver.recv().is_ok());
                }
                assert!(receiver.recv().is_err());
                let mut lock = finished_ok_copy.lock().unwrap();
                *lock = true;
            });

            for i in 0..HOW_MANY {
                mioco::spawn({
                    let sender = sender.clone();
                    move || {
                        sender.send(0usize).unwrap();
                        if i % 3 == 0 {
                            mioco::sleep_ms(10);
                        }

                        if i % 2 == 0 {
                            silent_panic()
                        }
                    }
                });
            }
        })
            .unwrap();

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn timer_times_out() {
    for &threads in THREADS_N.iter() {
        let finished_ok_1 = Arc::new(Mutex::new(false));
        let finished_ok_2 = Arc::new(Mutex::new(false));

        let finished_ok_1_copy = finished_ok_1.clone();
        let finished_ok_2_copy = finished_ok_2.clone();
        mioco::start_threads(threads, move || {

            let (reader, writer) = pipe();

            mioco::spawn(move || {
                let reader = reader;
                let mut timer = mioco::timer::Timer::new();
                timer.set_timeout(500);

                select!(
                    reader:r => { panic!("reader fired first!") },
                    timer:r => {},
                    );

                let mut lock = finished_ok_1_copy.lock().unwrap();
                *lock = true;
            });

            mioco::spawn(move || {
                let mut writer = writer;
                mioco::sleep_ms(1000);
                let _ = writer.write_all("test".as_bytes());

                let mut lock = finished_ok_2_copy.lock().unwrap();
                *lock = true;
            });
        })
            .unwrap();

        assert!(*finished_ok_1.lock().unwrap());
        assert!(*finished_ok_2.lock().unwrap());
    }
}

#[test]
fn timer_default_timeout() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_ok_copy = finished_ok.clone();
        mioco::start_threads(threads, move || {

            mioco::spawn(move || {
                let timer = mioco::timer::Timer::new();
                select!(
                    timer:r => {},
                    );

                let mut lock = finished_ok_copy.lock().unwrap();
                *lock = true;
            });
        })
            .unwrap();

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn sleep_takes_time() {
    for &threads in THREADS_N.iter() {
        let starting_time = SteadyTime::now();

        mioco::start_threads(threads, move || {
            mioco::sleep_ms(500);
        })
            .unwrap();

        assert!((SteadyTime::now() - starting_time) >= Duration::milliseconds(500));
    }
}

#[test]
fn timer_select_takes_time() {
    for &threads in THREADS_N.iter() {
        let starting_time = SteadyTime::now();

        mioco::start_threads(threads, move || {
            let mut timer = mioco::timer::Timer::new();
            timer.set_timeout(500);

            select!(
                timer:r => {},
                );
        })
            .unwrap();

        assert!((SteadyTime::now() - starting_time) >= Duration::milliseconds(500));
    }
}

#[test]
fn basic_timer_stress_test() {
    for &threads in THREADS_N.iter() {
        mioco::start_threads(threads, move || {
            for _ in 0..10 {
                for t in 0..100 {
                    mioco::spawn(move || {
                        mioco::sleep_ms(t);
                    });

                }

                mioco::sleep_ms(1);
            }
        })
            .unwrap();
    }
}

#[test]
fn exit_notifier_simple() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        mioco::start_threads(threads, move || {

            let notify = mioco::spawn(move || {});

            assert_eq!(notify.join().unwrap(), ());

            let mut lock = finished_copy.lock().unwrap();
            *lock = true;
        })
            .unwrap();

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn exit_notifier_simple_panic() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        mioco::start_threads(threads, move || {

            let notify = mioco::spawn(move || silent_panic());

            assert!(notify.join().is_err());

            let mut lock = finished_copy.lock().unwrap();
            *lock = true;
        })
            .unwrap();

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn tiny_stacks() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        let mut config = mioco::Config::new();

        config.set_thread_num(threads);
        unsafe {
            config.set_stack_size(1024 * 128);
        }

        let mut mioco = mioco::Mioco::new_configured(config);

        mioco.start(move || {
                 let mut lock = finished_copy.lock().unwrap();
                 *lock = true;
             })
             .unwrap();

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn basic_sync() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(true));

        let finished_copy = finished_ok.clone();

        mioco::start_threads(threads, move || {
            let res = mioco::sync(|| {
                mioco::sleep_ms(1000);
                let mut lock = finished_copy.lock().unwrap();
                assert_eq!(*lock, true);
                *lock = false;
                3u8
            });

            assert_eq!(res, 3u8);
            let mut lock = finished_copy.lock().unwrap();
            assert_eq!(*lock, false);
            *lock = true;
        })
            .unwrap();

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn sync_takes_time() {
    for &threads in THREADS_N.iter() {
        let starting_time = SteadyTime::now();

        mioco::start_threads(threads, move || {
            mioco::sync(|| {
                mioco::sleep_ms(500);
            });
        })
            .unwrap();

        assert!((SteadyTime::now() - starting_time) >= Duration::milliseconds(500));
    }
}

#[test]
fn basic_sync_in_loop() {
    for &threads in THREADS_N.iter() {
        mioco::start_threads(threads, move || {
            let mut counter = 0i32;
            for i in 0..10000 {
                let res = mioco::sync(|| {
                    if i & 0xf == 0 {
                        // cut the wait
                        mioco::sleep_ms(1);
                    }
                    counter += 1;
                    i
                });
                assert_eq!(res, i);
            }

            assert_eq!(counter, 10000);
        })
            .unwrap();
    }
}

#[test]
fn scheduler_kill_on_initial_drop() {
    struct TestScheduler;
    struct TestSchedulerThread;

    impl mioco::sched::Scheduler for TestScheduler {
        fn spawn_thread(&self) -> Box<mioco::sched::SchedulerThread> {
            Box::new(TestSchedulerThread)
        }
    }

    impl mioco::sched::SchedulerThread for TestSchedulerThread {
        fn spawned(&mut self,
                   _event_loop: &mut mioco::mio::EventLoop<mioco::Handler>,
                   _coroutine_ctrl: mioco::sched::Coroutine) {
            // drop
        }

        fn ready(&mut self,
                 _event_loop: &mut mioco::mio::EventLoop<mioco::Handler>,
                 _coroutine_ctrl: mioco::sched::Coroutine) {
            // drop
        }
    }

    let mut config = mioco::Config::new();
    config.set_scheduler(Box::new(TestScheduler));

    let mut mioco = mioco::Mioco::new_configured(config);

    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    let res = mioco.start(move || {
        let mut lock = finished_copy.lock().unwrap();
        *lock = true;
    });
    assert!(res.is_err());

    assert!(!*finished_ok.lock().unwrap());
}

#[test]
fn scheduler_kill_on_drop() {
    struct TestScheduler;
    struct TestSchedulerThread;

    impl mioco::sched::Scheduler for TestScheduler {
        fn spawn_thread(&self) -> Box<mioco::sched::SchedulerThread> {
            Box::new(TestSchedulerThread)
        }
    }

    impl mioco::sched::SchedulerThread for TestSchedulerThread {
        fn spawned(&mut self,
                   event_loop: &mut mioco::mio::EventLoop<mioco::Handler>,
                   coroutine_ctrl: mioco::sched::Coroutine) {
            coroutine_ctrl.resume(event_loop);
        }

        fn ready(&mut self,
                 _event_loop: &mut mioco::mio::EventLoop<mioco::Handler>,
                 _coroutine_ctrl: mioco::sched::Coroutine) {
            // drop
        }
    }

    let mut config = mioco::Config::new();
    config.set_scheduler(Box::new(TestScheduler));

    let mut mioco = mioco::Mioco::new_configured(config);

    let started_ok = Arc::new(Mutex::new(false));
    let finished_ok = Arc::new(Mutex::new(false));

    let started_copy = started_ok.clone();
    let finished_copy = finished_ok.clone();
    let res = mioco.start(move || {
        {
            let mut lock = started_copy.lock().unwrap();
            *lock = true;
        }
        mioco::sleep_ms(1000);
        let mut lock = finished_copy.lock().unwrap();
        *lock = true;
    });

    assert!(res.is_err());

    assert!(*started_ok.lock().unwrap());
    assert!(!*finished_ok.lock().unwrap());
}

#[test]
fn simple_yield() {
    struct TestScheduler;
    struct TestSchedulerThread;

    impl mioco::sched::Scheduler for TestScheduler {
        fn spawn_thread(&self) -> Box<mioco::sched::SchedulerThread> {
            Box::new(TestSchedulerThread)
        }
    }

    impl mioco::sched::SchedulerThread for TestSchedulerThread {
        fn spawned(&mut self,
                   event_loop: &mut mioco::mio::EventLoop<mioco::Handler>,
                   coroutine_ctrl: mioco::sched::Coroutine) {
            assert!(!coroutine_ctrl.is_yielding());
            coroutine_ctrl.resume(event_loop);
        }

        fn ready(&mut self,
                 event_loop: &mut mioco::mio::EventLoop<mioco::Handler>,
                 coroutine_ctrl: mioco::sched::Coroutine) {
            assert!(coroutine_ctrl.is_yielding());
            coroutine_ctrl.resume(event_loop);
        }
    }

    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();

        let mut config = mioco::Config::new();

        config.set_scheduler(Box::new(TestScheduler));
        config.set_thread_num(threads);

        let mut mioco = mioco::Mioco::new_configured(config);

        mioco.start(move || {
                 // long enough to test recursion exhausting stack
                 // small enough to finish in sane time
                 for _ in 0..10000 {
                     mioco::yield_now();
                 }
                 let mut lock = finished_copy.lock().unwrap();
                 *lock = true;
             })
             .unwrap();

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn spawn_as_start() {
    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    mioco::spawn(move || {
        let mut lock = finished_copy.lock().unwrap();
        *lock = true;
    });

    for _ in 0..60 {
        mioco::sleep(std::time::Duration::from_secs(1));
        if *finished_ok.lock().unwrap() {
            return;
        }
    }

    panic!("Coroutine never started?");
}

#[test]
#[cfg(target_arch = "x86_64")]
fn million_coroutines() {
    // actually less than million, to keep
    // the memory usage reasonable
    let mut config = mioco::Config::new();

    unsafe {
        config.set_stack_size(1024 * 16)
              .set_stack_protection(false);
        config.event_loop().timer_wheel_size(1024 * 256);
    }

    let mut mioco_server = mioco::Mioco::new_configured(config);

    let finished_ok = Arc::new(Mutex::new(false));
    let finished_copy = finished_ok.clone();

    mioco_server.start(move || {
                    for _ in 0..250 {
                        for _ in 0..1000 {
                            mioco::spawn(|| {
                                mioco::sleep_ms(5000);
                            });
                        }
                        // This is a workaround MIO queues becoming full
                        mioco::yield_now();
                    }
                    let mut lock = finished_copy.lock().unwrap();
                    *lock = true;
                })
                .unwrap();

    assert!(*finished_ok.lock().unwrap());
}

fn rwlock_wait_for_nonzero(counter : &mioco::sync::RwLock<usize>) {
    loop {
        {
            let counter = counter.read().unwrap();
            if *counter != 0 {
                break;
            }
        }
        mioco::sleep_ms(10)
    }
}

fn rwlock_wait_and_increment(counter : &mioco::sync::RwLock<usize>) {
    rwlock_wait_for_nonzero(counter);
    let mut counter = counter.write().unwrap();
    *counter = *counter + 1;
}

fn mutex_wait_for_nonzero(counter : &mioco::sync::Mutex<usize>) {
    loop {
        {
            let counter = counter.lock().unwrap();
            if *counter != 0 {
                break;
            }
        }
        mioco::sleep_ms(10)
    }
}

fn mutex_wait_and_increment(counter : &mioco::sync::Mutex<usize>) {
    mutex_wait_for_nonzero(counter);
    let mut counter = counter.lock().unwrap();
    *counter = *counter + 1;
}

#[test]
fn simple_rwlock() {
    for &threads in THREADS_N.iter() {
        let counter = Arc::new(mioco::sync::RwLock::new(0usize));
        let counter_copy = counter.clone();
        mioco::start_threads(threads, move || {
            for _ in 0..(threads * 4) {

                let counter = counter.clone();
                mioco::spawn(move || {
                    rwlock_wait_and_increment(&counter);
                });
            }
            mioco::sleep_ms(200);
            let mut counter = counter.write().unwrap();
            *counter = 1;
        })
            .unwrap();

        assert_eq!(*counter_copy.native_lock().read().unwrap(), (threads * 4) + 1);
    }
}

#[test]
fn simple_rwlock_supports_inside_and_outside() {
    for &threads in THREADS_N.iter() {
        let counter = Arc::new(mioco::sync::RwLock::new(0usize));
        let counter_copy = counter.clone();

        for _ in 0..(threads * 4) {
            let counter = counter.clone();
            std::thread::spawn(move || {
                rwlock_wait_and_increment(&*counter);
            });
        }

        mioco::start_threads(threads, move || {
            for _ in 0..(threads * 4) {
                let counter = counter.clone();
                mioco::spawn(move || {
                    rwlock_wait_and_increment(&counter);
                });
            }
            mioco::sleep_ms(200);
            let mut counter = counter.write().unwrap();
            *counter = 1;
        })
            .unwrap();

        assert_eq!(*counter_copy.native_lock().read().unwrap(), (threads * 8) + 1);
    }
}

#[test]
fn simple_mutex() {
    for &threads in THREADS_N.iter() {
        let counter = Arc::new(mioco::sync::Mutex::new(0usize));
        let counter_copy = counter.clone();
        mioco::start_threads(threads, move || {
            for _ in 0..(threads * 4) {
                let counter = counter.clone();
                mioco::spawn(move || {
                    mutex_wait_and_increment(&counter);
                });
            }
            mioco::sleep_ms(200);
            let mut counter = counter.lock().unwrap();
            *counter = 1;
        })
            .unwrap();

        assert_eq!(*counter_copy.native_lock().lock().unwrap(), (threads * 4) + 1);
    }
}

#[test]
fn simple_mutex_supports_inside_and_outside() {
    for &threads in THREADS_N.iter() {
        let counter = Arc::new(mioco::sync::Mutex::new(0usize));
        let counter_copy = counter.clone();

        for _ in 0..(threads * 4) {
            let counter = counter.clone();
            std::thread::spawn(move || {
                mutex_wait_and_increment(&*counter);
            });
        }
        mioco::start_threads(threads, move || {
            for _ in 0..(threads * 4) {
                let counter = counter.clone();
                mioco::spawn(move || {
                    mutex_wait_and_increment(&counter);
                });
            }
            mioco::sleep_ms(200);
            let mut counter = counter.lock().unwrap();
            *counter = 1;
        })
            .unwrap();

        assert_eq!(*counter_copy.native_lock().lock().unwrap(), (threads * 8) + 1);
    }
}

#[test]
#[cfg_attr(windows, ignore)] // Issue #105
fn tcp_basic_client_server() {
    use std::str::FromStr;
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        mioco::start_threads(threads, move || {

            let (out, inn) = mioco::sync::mpsc::channel();

            mioco::spawn(move || {
                let addr = FromStr::from_str("127.0.0.1:0").unwrap();
                let listener = mioco::tcp::TcpListener::bind(&addr).unwrap();

                out.send(listener.local_addr().unwrap()).unwrap();

                for i in 0..2 {
                    let mut conn = listener.accept().unwrap();
                    let mut buf = [0u8; 1024];
                    let size = conn.read(&mut buf).unwrap();
                    assert_eq!(size, 11);

                    let mut lock = finished_copy.lock().unwrap();
                    *lock = i == 1;
                }
            });

            mioco::spawn(move || {
                let addr = inn.recv().unwrap();

                let stream = mioco::tcp::TcpStream::connect(&addr).unwrap();
                stream.try_write(b"Hello world").unwrap().unwrap();

                let sock = match addr {
                               SocketAddr::V4(..) => TcpBuilder::new_v4(),
                               SocketAddr::V6(..) => TcpBuilder::new_v6(),
                           }
                           .unwrap();
                let stream = mioco::tcp::TcpStream::connect_stream(sock.to_tcp_stream().unwrap(),
                                                                   &addr)
                                 .unwrap();
                stream.try_write(b"Hello world").unwrap().unwrap();
            });
        })
            .unwrap();

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn simple_userdata() {
    for &threads in THREADS_N.iter() {
        mioco::start_threads(threads, || {
            mioco::set_userdata(42 as u32);
            assert_eq!(*mioco::get_userdata::<u32>().unwrap(), 42);
        })
            .unwrap();
    }
}

#[test]
fn userdata_wrong_type() {
    for &threads in THREADS_N.iter() {
        mioco::start_threads(threads, || {
            mioco::set_userdata(42 as u32);
            assert_eq!(mioco::get_userdata::<i32>(), None);
        })
            .unwrap()
    }
}

#[test]
fn userdata_scheduler() {
    struct TestScheduler;
    struct TestSchedulerThread;

    impl mioco::sched::Scheduler for TestScheduler {
        fn spawn_thread(&self) -> Box<mioco::sched::SchedulerThread> {
            Box::new(TestSchedulerThread)
        }
    }

    impl mioco::sched::SchedulerThread for TestSchedulerThread {
        fn spawned(&mut self,
                   _event_loop: &mut mioco::mio::EventLoop<mioco::Handler>,
                   _coroutine_ctrl: mioco::sched::Coroutine) {
            assert_eq!(*_coroutine_ctrl.get_userdata::<u32>().unwrap(), 42)
            // drop
        }

        fn ready(&mut self,
                 _event_loop: &mut mioco::mio::EventLoop<mioco::Handler>,
                 _coroutine_ctrl: mioco::sched::Coroutine) {
            // drop
        }
    }

    let mut config = mioco::Config::new();
    config.set_userdata(42 as u32);
    config.set_scheduler(Box::new(TestScheduler));

    let mut mioco = mioco::Mioco::new_configured(config);

    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    let res = mioco.start(move || {
        let mut lock = finished_copy.lock().unwrap();
        *lock = true;
    });

    assert!(res.is_err());

    assert!(!*finished_ok.lock().unwrap());
}

#[test]
fn simple_userdata_inheritance() {
    for &threads in THREADS_N.iter() {
        mioco::start_threads(threads, || {
            mioco::set_children_userdata(Some(42 as u32));
            mioco::spawn(|| {
                assert_eq!(*mioco::get_userdata::<u32>().unwrap(), 42);
            });
        })
            .unwrap()
    }
}

#[test]
fn no_userdata_inheritance() {
    for &threads in THREADS_N.iter() {
        mioco::start_threads(threads, || {
            mioco::spawn(|| {
                assert_eq!(mioco::get_userdata::<u32>(), None);
            });
        })
            .unwrap()
    }
}

#[test]
fn userdata_multi_inheritance() {
    for &threads in THREADS_N.iter() {
        mioco::start_threads(threads, || {
            mioco::set_children_userdata(Some(42 as u32));
            mioco::spawn(|| {
                mioco::spawn(|| {
                    assert_eq!(*mioco::get_userdata::<u32>().unwrap(), 42);
                });
            });
        })
            .unwrap()
    }
}

#[test]
fn userdata_inheritance_reset() {
    for &threads in THREADS_N.iter() {
        mioco::start_threads(threads, || {
            mioco::set_children_userdata(Some(42 as u32));
            mioco::spawn(|| {
                mioco::set_children_userdata::<u32>(None);
                mioco::spawn(|| {
                    assert_eq!(mioco::get_userdata::<u32>(), None);
                });
            });
        })
            .unwrap()
    }
}

#[test]
fn userdata_no_reference_invalidation() {
    for &threads in THREADS_N.iter() {
        mioco::start_threads(threads, || {
            mioco::set_userdata(42 as u32);
            let reference = mioco::get_userdata::<u32>().unwrap();
            mioco::set_userdata(41 as u32);
            assert_eq!(*reference, 42);
            assert_eq!(*mioco::get_userdata::<u32>().unwrap(), 41);
        })
            .unwrap()
    }
}

#[test]
fn in_coroutine_true() {
    mioco::start(|| {
        assert!(mioco::in_coroutine());
    })
        .unwrap();
}

#[test]
fn in_coroutine_false() {
    assert!(!mioco::in_coroutine());
}

#[test]
fn mpsc_outside_outside() {
    let (tx1, rx1) = mioco::sync::mpsc::channel();
    let (tx2, rx2) = mioco::sync::mpsc::channel();
    thread::spawn(move || {
        for i in 0..10 {
            tx1.send(i).unwrap();
            assert_eq!(rx2.recv().unwrap(), i);
        }
    });
    for i in 0..10 {
        assert_eq!(rx1.recv().unwrap(), i);
        tx2.send(i).unwrap();
    }
}


#[test]
fn mpsc_inside_outside() {
    let (tx1, rx1) = mioco::sync::mpsc::channel();
    let (tx2, rx2) = mioco::sync::mpsc::channel();
    mioco::spawn(move || {
        for i in 0..10 {
            tx1.send(i).unwrap();
            assert_eq!(rx2.recv().unwrap(), i);
        }
    });
    for i in 0..10 {
        assert_eq!(rx1.recv().unwrap(), i);
        tx2.send(i).unwrap();
    }
}


#[test]
fn mpsc_inside_inside() {
    for &threads in THREADS_N.iter() {
        let (tx1, rx1) = mioco::sync::mpsc::channel();
        let (tx2, rx2) = mioco::sync::mpsc::channel();

        let finished_ok1 = Arc::new(Mutex::new(false));
        let finished_copy1 = finished_ok1.clone();

        let finished_ok2 = Arc::new(Mutex::new(false));
        let finished_copy2 = finished_ok2.clone();

        mioco::start_threads(threads, move || {
            mioco::spawn(move || {
                for i in 0..10 {
                    tx1.send(i).unwrap();
                    assert_eq!(rx2.recv().unwrap(), i);
                }
                let mut lock = finished_copy1.lock().unwrap();
                *lock = true;
            });
            mioco::spawn(move || {
                for i in 0..10 {
                    assert_eq!(rx1.recv().unwrap(), i);
                    tx2.send(i).unwrap();
                }
                let mut lock = finished_copy2.lock().unwrap();
                *lock = true;
            });
        })
            .unwrap();

        assert!(*finished_ok1.lock().unwrap());
        assert!(*finished_ok2.lock().unwrap());
    }
}

#[test]
fn simple_shutdown() {
    for &threads in THREADS_N.iter() {
        let res = mioco::start_threads(threads, move || {
            for _ in 0..1024 {
                mioco::spawn(move || {
                    loop {
                        mioco::yield_now();
                    }
                });
            }

            for _ in 0..16 {
                mioco::yield_now();
            }
            mioco::shutdown();
        });
        assert!(res.is_err());
    }
}

#[test]
fn simple_shutdown_on_blocked() {
    for &threads in THREADS_N.iter() {
        let res = mioco::start_threads(threads, move || {
            for _ in 0..1024 {
                mioco::spawn(move || {
                    loop {
                        let (mut reader, _writer) = pipe();
                        let mut buf = [0u8; 16];
                        let _ = reader.read(&mut buf);
                    }
                });
            }

            for _ in 0..2 {
                mioco::yield_now();
            }

            mioco::shutdown();
        });
        assert!(res.is_err());
    }
}

#[test]
fn empty_shutdown() {
    for &threads in THREADS_N.iter() {
        let res = mioco::start_threads(threads, move || {
            mioco::shutdown();
        });

        assert!(res.is_err());
    }
}

#[test]
fn empty_shutdown_2() {
    for &threads in THREADS_N.iter() {
        let res = mioco::start_threads(threads, move || {
            for _ in 0..16 {
                mioco::spawn(move || {});
            }
            mioco::shutdown();
        });

        assert!(res.is_err());
    }
}

#[test]
fn multiple_return_types() {
    for &threads in THREADS_N.iter() {
        let res = mioco::start_threads(threads, move || {
            let join1 = mioco::spawn(move || 3);
            let join2 = mioco::spawn(move || "foobar");

            assert_eq!(join1.join().unwrap(), 3);
            assert_eq!(join2.join().unwrap(), "foobar");

            vec![1, 2]
        });

        assert_eq!(res.unwrap(), vec![1, 2]);
    }

}
