use super::*;

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use time::{SteadyTime, Duration};

use std::thread;

const THREADS_N : [usize; 4] = [1, 2, 5, 21];

#[test]
fn empty_handler() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        start_threads(threads, move |_| {
            let mut lock = finished_copy.lock().unwrap();
            *lock = true;

            Ok(())
        });

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn empty_subcoroutines() {
    for &threads in THREADS_N.iter() {
        let counter = Arc::new(Mutex::new(0i32));

        let counter_copy = counter.clone();

        start_threads(threads, move |mioco| {

            for _ in 0..512 {
                let counter_subcopy = counter_copy.clone();
                mioco.spawn(move |_| {
                    let mut lock = counter_subcopy.lock().unwrap();
                    *lock += 1;

                    Ok(())
                });
            }

            let mut lock = counter_copy.lock().unwrap();
            *lock += 1;

            Ok(())
        });

        assert_eq!(*counter.lock().unwrap(), 512 + 1);
    }
}

#[test]
fn contain_panics() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        start_threads(threads, move |_| {
            panic!()
        });

        assert!(!*finished_ok.lock().unwrap());
    }
}

#[test]
fn contain_panics_in_subcoroutines() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        start_threads(threads, move |mioco| {

            for _ in 0..512 {
                mioco.spawn(|_| {
                    panic!()
                });
            }

            let mut lock = finished_copy.lock().unwrap();
            *lock = true;

            Ok(())
        });

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn long_chain() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        start_threads(threads, move |mioco| {

            let (first_reader, first_writer) = try!(mio::unix::pipe());

            let mut prev_reader = first_reader;

            // TODO: increase after https://github.com/dpc/mioco/issues/8 is fixed
            for _ in 0..128 {
                let (reader, writer) = try!(mio::unix::pipe());

                mioco.spawn(move |mioco| {
                    let mut reader = mioco.wrap(prev_reader);
                    let mut writer = mioco.wrap(writer);

                    let _ = std::io::copy(&mut reader, &mut writer);

                    Ok(())
                });

                prev_reader = reader;
            }

            let mut first_writer = mioco.wrap(first_writer);
            let mut last_reader = mioco.wrap(prev_reader);

            for i in 0..9 {
                let test_str = format!("TeSt{}", i);
                let _ = first_writer.write_all(test_str.as_str().as_bytes());
                let mut buf = [0u8; 16];

                let _ = last_reader.read(&mut buf);

                if &buf[0..5] != test_str.as_str().as_bytes() {
                    panic!();
                }
            }

            let mut lock = finished_copy.lock().unwrap();
            *lock = true;

            Ok(())
        });

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn lots_of_event_sources() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        start_threads(threads, move |mioco| {

            let (first_reader, first_writer) = try!(mio::unix::pipe());

            let mut prev_reader = first_reader;

            // TODO: increase after https://github.com/dpc/mioco/issues/8 is fixed
            for _ in 0..4 {
                let (reader, writer) = try!(mio::unix::pipe());

                mioco.spawn(move |mioco| {
                    // This fake readers are not really used, they are just registered for the sake of
                    // testing if event sources registered with high id number are handled correctly
                    let mut readers = Vec::new();
                    let mut writers = Vec::new();
                    for _ in 0..100 {
                        let (r, w) = try!(mio::unix::pipe());
                        readers.push(mioco.wrap(r));
                        writers.push(mioco.wrap(w));
                    }

                    let mut reader = mioco.wrap(prev_reader);
                    let mut writer = mioco.wrap(writer);

                    let _ = std::io::copy(&mut reader, &mut writer);

                    Ok(())
                });

                prev_reader = reader;
            }

            let mut first_writer = mioco.wrap(first_writer);
            let mut last_reader = mioco.wrap(prev_reader);

            for i in 0..9 {
                let test_str = format!("TeSt{}", i);
                let _ = first_writer.write_all(test_str.as_str().as_bytes());
                let mut buf = [0u8; 16];

                let _ = last_reader.read(&mut buf);

                if &buf[0..5] != test_str.as_str().as_bytes() {
                    panic!();
                }
            }

            let mut lock = finished_copy.lock().unwrap();
            *lock = true;

            Ok(())
        });

        assert!(*finished_ok.lock().unwrap());
    }
}

/// Test if drop is performed on IOs when coroutine panics.
#[test]
fn destructs_io_on_panic() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_ok_copy = finished_ok.clone();
        start_threads(threads, move |mioco| {

            let (reader, writer) = try!(mio::unix::pipe());

            mioco.spawn(move |mioco| {
                let mut reader = mioco.wrap(reader);
                let mut buf = [0u8; 16];
                let ret = reader.read(&mut buf);
                assert!(ret.is_ok());

                let mut lock = finished_ok_copy.lock().unwrap();
                *lock = true;
                Ok(())
            });

            mioco.spawn(move |mioco| {
                let _writer = mioco.wrap(writer);
                panic!();
            });


            Ok(())
        });

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
        start_threads(threads, move |mioco| {

            let (reader, writer) = try!(mio::unix::pipe());

            mioco.spawn(move |mioco| {
                let reader = mioco.wrap(reader);
                let timer_id = mioco.timer().id();
                mioco.timer().set_timeout(500);
                let ev = mioco.select_read_from(&[reader.id(), timer_id]);
                assert_eq!(ev.id(), timer_id);

                let mut lock = finished_ok_1_copy.lock().unwrap();
                *lock = true;
                Ok(())
            });

            mioco.spawn(move |mioco| {
                let mut writer = mioco.wrap(writer);
                mioco.sleep(1000);
                let _ = writer.write_all("test".as_bytes());

                let mut lock = finished_ok_2_copy.lock().unwrap();
                *lock = true;
                Ok(())
            });


            Ok(())
        });

        assert!(*finished_ok_1.lock().unwrap());
        assert!(*finished_ok_2.lock().unwrap());
    }
}

#[test]
fn timer_default_timeout() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_ok_copy = finished_ok.clone();
        start_threads(threads, move |mioco| {

            mioco.spawn(move |mioco| {
                let timer_id = mioco.timer().id();
                let ev = mioco.select_read_from(&[timer_id]);
                assert_eq!(ev.id(), timer_id);

                let mut lock = finished_ok_copy.lock().unwrap();
                *lock = true;
                Ok(())
            });

            Ok(())
        });

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn sleep_takes_time() {
    for &threads in THREADS_N.iter() {
        let starting_time = SteadyTime::now();

        start_threads(threads, move |mioco| {
            mioco.sleep(500); Ok(())
        });

        assert!((SteadyTime::now() - starting_time) >= Duration::milliseconds(500));
    }
}

#[test]
fn timer_select_takes_time() {
    for &threads in THREADS_N.iter() {
        let starting_time = SteadyTime::now();

        start_threads(threads, move |mioco| {
            let timer_id = mioco.timer().id();
            mioco.timer().set_timeout(500);
            let ev = mioco.select_read_from(&[timer_id]);
            assert_eq!(mioco.timer().id(), ev.id());
            Ok(())
        });

        assert!((SteadyTime::now() - starting_time) >= Duration::milliseconds(500));
    }
}

#[test]
fn basic_timer_stress_test() {
    for &threads in THREADS_N.iter() {
        start_threads(threads, move |mioco| {
            for _ in 0..10 {
                for t in 0..100 {
                    mioco.spawn(move |mioco| {
                        mioco.sleep(t);
                        Ok(())
                    });

                }

                mioco.sleep(1);
            }
            Ok(())
        });
    }
}

#[test]
fn exit_notifier_simple() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        start_threads(threads, move |mioco| {

            let notify = mioco.spawn(move |_| {
                Ok(())
            }).exit_notificator();

            let notify = mioco.wrap(notify);

            assert!(!notify.read().is_panic());

            let mut lock = finished_copy.lock().unwrap();
            *lock = true;
            Ok(())
        });

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn exit_notifier_simple_panic() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        start_threads(threads, move |mioco| {

            let notify = mioco.spawn(move |_| {
                panic!()
            }).exit_notificator();

            let notify = mioco.wrap(notify);

            assert!(notify.read().is_panic());

            let mut lock = finished_copy.lock().unwrap();
            *lock = true;
            Ok(())
        });

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn exit_notifier_wrap_after_finish() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        start_threads(threads, move |mioco| {

            let handle1 = mioco.spawn(move |_| {
                panic!()
            });

            mioco.sleep(1000);
            let notify1 = handle1.exit_notificator();

            let handle2 = mioco.spawn(move |mioco| {
                let notify1 = mioco.wrap(notify1);
                assert!(notify1.read().is_panic());
                Ok(())
            });

            let notify2 = handle2.exit_notificator();
            let notify2 = mioco.wrap(notify2);
            assert!(!notify2.read().is_panic());


            let notify1 = handle1.exit_notificator();
            let notify1 = mioco.wrap(notify1);
            assert!(notify1.read().is_panic());


            let mut lock = finished_copy.lock().unwrap();
            *lock = true;
            Ok(())
        });

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn tiny_stacks() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        let mut config = Config::new();

        config.set_thread_num(threads);
        unsafe { config.set_stack_size(1024 * 128); }

        let mut mioco = Mioco::new_configured(config);

        mioco.start(move |_| {
            let mut lock = finished_copy.lock().unwrap();
            *lock = true;
            Ok(())
        });

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn basic_sync() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(true));

        let finished_copy = finished_ok.clone();

        start_threads(threads, move |mioco| {
            let res = mioco.sync(|| {
                thread::sleep_ms(1000);
                let mut lock = finished_copy.lock().unwrap();
                assert_eq!(*lock, true);
                *lock = false;
                3u8
            });

            assert_eq!(res, 3u8);
            let mut lock = finished_copy.lock().unwrap();
            assert_eq!(*lock, false);
            *lock = true;
            Ok(())
        });

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn sync_takes_time() {
    for &threads in THREADS_N.iter() {
        let starting_time = SteadyTime::now();

        start_threads(threads, move |mioco| {
            mioco.sync(|| {
                thread::sleep_ms(500);
            });
            Ok(())
        });

        assert!((SteadyTime::now() - starting_time) >= Duration::milliseconds(500));
    }
}

#[test]
fn basic_sync_in_loop() {
    for &threads in THREADS_N.iter() {
        start_threads(threads, move |mioco| {
            let mut counter = 0i32;
            for i in 0..10000 {
                let res = mioco.sync(|| {
                    if i & 0xf == 0 { // cut the wait
                        thread::sleep_ms(1);
                    }
                    counter += 1;
                    i
                });
                assert_eq!(res, i);
            }

            assert_eq!(counter, 10000);
            Ok(())
        });
    }
}
#[test]
fn scheduler_kill_on_initial_drop() {
    struct TestScheduler;
    struct TestSchedulerThread;

    impl Scheduler for TestScheduler {
        fn spawn_thread(&mut self) -> Box<SchedulerThread> {
            Box::new(TestSchedulerThread)
        }
    }

    impl SchedulerThread for TestSchedulerThread {
        fn spawned(&mut self, _event_loop: &mut mio::EventLoop<Handler>, _coroutine_ctrl: CoroutineControl) {
            // drop
        }

        fn ready(&mut self, _event_loop: &mut mio::EventLoop<Handler>, _coroutine_ctrl: CoroutineControl) {
            // drop
        }
    }

    let mut config = Config::new();
    config.set_scheduler(Box::new(TestScheduler));

    let mut mioco = Mioco::new_configured(config);

    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    mioco.start(move |_| {
        let mut lock = finished_copy.lock().unwrap();
        *lock = true;
        Ok(())
    });

    assert!(!*finished_ok.lock().unwrap());
}

#[test]
fn scheduler_kill_on_drop() {
    struct TestScheduler;
    struct TestSchedulerThread;

    impl Scheduler for TestScheduler {
        fn spawn_thread(&mut self) -> Box<SchedulerThread> {
            Box::new(TestSchedulerThread)
        }
    }

    impl SchedulerThread for TestSchedulerThread {
        fn spawned(&mut self, event_loop: &mut mio::EventLoop<Handler>, coroutine_ctrl: CoroutineControl) {
            coroutine_ctrl.resume(event_loop);
        }

        fn ready(&mut self, _event_loop: &mut mio::EventLoop<Handler>, _coroutine_ctrl: CoroutineControl) {
            // drop
        }
    }

    let mut config = Config::new();
    config.set_scheduler(Box::new(TestScheduler));

    let mut mioco = Mioco::new_configured(config);

    let started_ok = Arc::new(Mutex::new(false));
    let finished_ok = Arc::new(Mutex::new(false));

    let started_copy = started_ok.clone();
    let finished_copy = finished_ok.clone();
    mioco.start(move |mioco| {
        {
            let mut lock = started_copy.lock().unwrap();
            *lock = true;
        }
        mioco.sleep(1000);
        let mut lock = finished_copy.lock().unwrap();
        *lock = true;
        Ok(())
    });

    assert!(*started_ok.lock().unwrap());
    assert!(!*finished_ok.lock().unwrap());
}

#[test]
fn simple_yield() {
    struct TestScheduler;
    struct TestSchedulerThread;

    impl Scheduler for TestScheduler {
        fn spawn_thread(&mut self) -> Box<SchedulerThread> {
            Box::new(TestSchedulerThread)
        }
    }

    impl SchedulerThread for TestSchedulerThread {
        fn spawned(&mut self, event_loop: &mut mio::EventLoop<Handler>, coroutine_ctrl: CoroutineControl) {
            assert!(!coroutine_ctrl.is_yielding());
            coroutine_ctrl.resume(event_loop);
        }

        fn ready(&mut self, event_loop: &mut mio::EventLoop<Handler>, coroutine_ctrl: CoroutineControl) {
            assert!(coroutine_ctrl.is_yielding());
            coroutine_ctrl.resume(event_loop);
        }
    }

    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();

        let mut config = Config::new();

        config.set_scheduler(Box::new(TestScheduler));
        config.set_thread_num(threads);

        let mut mioco = Mioco::new_configured(config);

        mioco.start(move |mioco| {
            // long enough to test recursion exhausting stack
            // small enough to finish in sane time
            for _ in 0..10000 {
                mioco.yield_now();
            }
            let mut lock = finished_copy.lock().unwrap();
            *lock = true;
            Ok(())
        });

        assert!(*finished_ok.lock().unwrap());
    }
}

#[test]
fn simple_unwrap() {
    for &threads in THREADS_N.iter() {
        let finished_ok = Arc::new(Mutex::new(false));

        let finished_copy = finished_ok.clone();
        start_threads(threads, move |mioco| {
            let (reader, writer) = try!(mio::unix::pipe());

            mioco.spawn(move |mioco| {

                let reader = mioco.wrap(reader);
                let reader = mioco.unwrap(reader);

                mioco.spawn(move |mioco| {

                    let reader = mioco.wrap(reader);
                    mioco.select_read_from(&[reader.id()]);
                    let reader = mioco.unwrap(reader);

                    mioco.spawn(move |mioco| {

                        let mut reader = mioco.wrap(reader);
                        let mut buf = [0u8, 8];
                        let _ = reader.read(&mut buf);
                        let _ = mioco.unwrap(reader);

                        let mut lock = finished_copy.lock().unwrap();
                        *lock = true;
                        Ok(())
                    });
                    Ok(())
                });
                Ok(())
            });

            let mut writer = mioco.wrap(writer);
            for _ in 0..100 {
                match writer.write(b"x") {
                    Err(_) => break,
                    Ok(_) => {},
                }
            }
            Ok(())

        });

        assert!(*finished_ok.lock().unwrap());
    }
}

