use super::{start};

use std;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use mio;

#[test]
fn empty_handler() {
    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    start(move |_| {
        let mut lock = finished_copy.lock().unwrap();
        *lock = true;

        Ok(())
    });

    assert!(*finished_ok.lock().unwrap());
}

#[test]
fn empty_subcoroutines() {
    let counter = Arc::new(Mutex::new(0i32));

    let counter_copy = counter.clone();

    start(move |mioco| {

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

#[test]
fn contain_panics() {
    let finished_ok = Arc::new(Mutex::new(false));

    start(move |_| {
        panic!()
    });

    assert!(!*finished_ok.lock().unwrap());
}

#[test]
fn contain_panics_in_subcoroutines() {
    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    start(move |mioco| {

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

#[test]
fn long_chain() {
    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    start(move |mioco| {

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

#[test]
fn lots_of_event_sources() {
    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    start(move |mioco| {

        let (first_reader, first_writer) = try!(mio::unix::pipe());

        let mut prev_reader = first_reader;

        // TODO: increase after https://github.com/dpc/mioco/issues/8 is fixed
        for _ in 0..4 {
            let (reader, writer) = try!(mio::unix::pipe());

            mioco.spawn(move |mioco| {
                // This fake readers are not really used, they are just registered for the sake of
                // testing if event sources registered with high index number are handled correctly
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

#[test]
fn timer_times_out() {
    let finished_ok_1 = Arc::new(Mutex::new(false));
    let finished_ok_2 = Arc::new(Mutex::new(false));

    let finished_ok_1_copy = finished_ok_1.clone();
    let finished_ok_2_copy = finished_ok_2.clone();
    start(move |mioco| {

        let (reader, writer) = try!(mio::unix::pipe());

        mioco.spawn(move |mioco| {
            let reader = mioco.wrap(reader);
            let timer = mioco.timeout(50);
            let ev = mioco.select_read_from(&[reader.index(), timer.index()]);
            assert_eq!(ev.index(), timer.index());

            let mut lock = finished_ok_1_copy.lock().unwrap();
            *lock = true;
            Ok(())
        });

        mioco.spawn(move |mioco| {
            let mut writer = mioco.wrap(writer);
            mioco.sleep(100);
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

#[test]
fn timer_can_be_reused() {
    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    start(move |mioco| {

        mioco.spawn(move |mioco| {
            let timer = mioco.timeout(50);

            mioco.select_read_from(&[timer.index()]);
            timer.reset();

            mioco.select_read_from(&[timer.index()]);

            let mut lock = finished_copy.lock().unwrap();
            *lock = true;

            Ok(())
        });

        Ok(())
    });

    assert!(*finished_ok.lock().unwrap());
}

#[test]
fn exit_notifier_simple() {
    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    start(move |mioco| {

        let notify = mioco.spawn(move |_| {
            Ok(())
        }).exit_notificator();

        let mut notify = mioco.wrap(notify);

        assert!(!notify.recv().unwrap().is_panic());

        let mut lock = finished_copy.lock().unwrap();
        *lock = true;
        Ok(())
    });

    assert!(*finished_ok.lock().unwrap());
}

#[test]
fn exit_notifier_simple_panic() {
    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    start(move |mioco| {

        let notify = mioco.spawn(move |_| {
            panic!()
        }).exit_notificator();

        let mut notify = mioco.wrap(notify);

        assert!(notify.recv().unwrap().is_panic());

        let mut lock = finished_copy.lock().unwrap();
        *lock = true;
        Ok(())
    });

    assert!(*finished_ok.lock().unwrap());
}


#[test]
fn exit_notifier_wrap_after_finish() {
    let finished_ok = Arc::new(Mutex::new(false));

    let finished_copy = finished_ok.clone();
    start(move |mioco| {

        let handle1 = mioco.spawn(move |_| {
            panic!()
        });

        let notify1 = handle1.exit_notificator();

        let handle2 = mioco.spawn(move |mioco| {
            let mut notify1 = mioco.wrap(notify1);
            assert!(notify1.recv().unwrap().is_panic());
            Ok(())
        });

        let notify2 = handle2.exit_notificator();
        let mut notify2 = mioco.wrap(notify2);
        assert!(!notify2.recv().unwrap().is_panic());


        let notify1 = handle1.exit_notificator();
        let mut notify1 = mioco.wrap(notify1);
        assert!(notify1.recv().unwrap().is_panic());


        let mut lock = finished_copy.lock().unwrap();
        *lock = true;
        Ok(())
    });

    assert!(*finished_ok.lock().unwrap());
}
