use super::start;

use std::sync::{Arc, Mutex};

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
