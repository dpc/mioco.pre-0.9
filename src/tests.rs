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
