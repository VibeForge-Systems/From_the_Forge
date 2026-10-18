//! M3 §16.6: the panic hook writes the thread name and the location, never
//! the payload. Its own test binary, because the hook is process-wide.

use std::sync::{Arc, Mutex};

use openmessage_archive_lib::panic_hook;

const SENTINEL: &str = "SYNTH-BODY-panic-payload-sentinel";

#[test]
fn the_hook_never_writes_the_payload() {
    let out: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = out.clone();
    std::panic::set_hook(panic_hook::hook(move |l| {
        sink.lock().expect("lock").push(l.to_owned());
    }));

    let payloads: Vec<Box<dyn FnOnce() + Send>> = vec![
        Box::new(|| panic!("{SENTINEL}")),
        Box::new(|| panic!("static payload SYNTH-BODY-panic-payload-sentinel")),
        Box::new(|| std::panic::panic_any(SENTINEL.to_owned())),
        Box::new(|| std::panic::panic_any(42_u32)),
    ];
    let count = payloads.len();
    for (i, f) in payloads.into_iter().enumerate() {
        let joined = std::thread::Builder::new()
            .name(format!("oma-job-test-{i}"))
            .spawn(f)
            .expect("spawn")
            .join();
        assert!(joined.is_err(), "the thread panicked");
    }
    let _ = std::panic::take_hook();

    let lines = out.lock().expect("lock").clone();
    assert_eq!(lines.len(), count, "{lines:?}");
    for (i, l) in lines.iter().enumerate() {
        assert!(!l.contains(SENTINEL), "the payload leaked: {l}");
        assert!(l.contains(&format!("oma-job-test-{i}")), "{l}");
        assert!(l.contains("panic_hook.rs:"), "{l}");
    }
}
