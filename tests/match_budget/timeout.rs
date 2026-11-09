//! A real layer's match must run inside the host's configured budget. Hold its tuple
//! decoder until after the timeout, rather than assume a SPARQL query is slow enough.

#[path = "../common/mod.rs"]
mod common;

use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use common::*;
use ikigai_core::{Error, Verb};
use ikigai_intray::TupleCodec;

use crate::at_rest::Sealer;
use crate::host::ModelState;

struct HeldCodec {
    sealer: Sealer,
    entered: mpsc::Sender<()>,
    held: Mutex<mpsc::Receiver<()>>,
}

impl TupleCodec for HeldCodec {
    fn encode(&self, plain: &[u8]) -> Result<Vec<u8>, String> {
        self.sealer.seal(plain)
    }

    fn decode(&self, stored: &[u8]) -> Result<Vec<u8>, String> {
        self.entered.send(()).expect("the test waits for decoding");
        let _ = self.held.lock().expect("decoder lock").recv();
        self.sealer.open(stored).map_err(|why| format!("{why:?}"))
    }

    fn is_current(&self, stored: &[u8]) -> bool {
        self.sealer.is_current(stored)
    }
}

#[test]
fn a_real_layer_match_times_out_while_its_decoder_is_held() {
    let mut f = fixture();
    f.config.write(
        "layers/tasks-bobby.toml",
        "model = \"tuplespace\"\nlabel = \"Tasks\"\nshapes = \"tasks.shapes.ttl\"\n\
         context = \"tasks.context.jsonld\"\naddressed = true\nmatch = true\n",
    );
    f.config.write(
        "layers.toml",
        "[match]\nbudget_ms = 1000\nmax_overdue = 1\n",
    );
    f.reopen();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();

    // Remove the kernel's reference while installing a test-only codec on the actual
    // layer. Its endpoint, budget, authority and stored tuple remain the host's own.
    let Fixture {
        config,
        state,
        mut host,
        kernel,
    } = f;
    drop(kernel);
    let (entered, decoding) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let layer = Arc::get_mut(
        Arc::get_mut(&mut host)
            .unwrap()
            .layers
            .get_mut("tasks-bobby")
            .unwrap(),
    )
    .unwrap();
    let ModelState::Tuplespace(space) = &mut layer.model else {
        panic!("a tuplespace layer")
    };
    space.store.set_test_codec(Arc::new(HeldCodec {
        sealer: Sealer::new(&key(config.path())),
        entered,
        held: Mutex::new(held),
    }));
    let kernel = kernel_over(&host);
    let (answer, answered) = mpsc::channel();
    let read = std::thread::spawn(move || {
        let result = issue(
            &kernel,
            Verb::Source,
            "urn:layer:tasks-bobby:tuples",
            &[("match", "ASK { ?s ?p ?o }")],
            &tasker(&participant("bobby")),
        );
        let _ = answer.send(result);
    });
    decoding
        .recv_timeout(Duration::from_secs(30))
        .expect("the real match reached tuple decoding");
    let result = answered
        .recv_timeout(Duration::from_secs(30))
        .expect("the caller times out while decoding is still held");
    read.join().expect("the read thread");
    assert!(
        matches!(result, Err(Error::Timeout(ref message)) if message.contains("1000 ms")),
        "{result:?}"
    );
    assert_eq!(
        host.match_budget().overdue_evaluations(),
        1,
        "the real match is still held after its caller timed out"
    );
    drop(release);
    // Keep the fixture directories alive until the worker has finished reading them.
    let deadline = now();
    while host.match_budget().overdue_evaluations() != 0 {
        assert!(
            deadline.elapsed() < Duration::from_secs(30),
            "the released match did not end"
        );
        std::thread::yield_now();
    }
    drop((host, state, config));
}

// Wall time only bounds cleanup if the released worker is broken.
#[allow(clippy::disallowed_methods)]
fn now() -> std::time::Instant {
    std::time::Instant::now()
}
