use std::cell::OnceCell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use rustradio::blockchain;
use rustradio::blocks::*;
use rustradio::graph::GraphRunner;
use rustradio::stream::ReadStream;

use log::{info, trace};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

use crate::ReceiverId;
use crate::js_performance_now;
use crate::wasm_source;
use crate::{MainToWorker, WorkerToMain};

// TODO: magic values.
const SOURCE_CHANNEL_SIZE: usize = 10;

struct GraphComms {
    src: HashMap<ReceiverId, async_channel::Sender<crate::wasm_source::Msg>>,
    graph: async_channel::Sender<()>,
}

thread_local! {
    // TODO: switch to async_std::Mutex? But on top of that, there's no
    // guarantee they wake up in order, right? So RefCell for the buffer, then
    // async mutex for sending the messages?
    //
    // Maybe use futures_intrusive::LocalMutex with `is_fair`, which does guarantee FIFO?
    static GRAPH_COMMS: OnceCell<Rc<RefCell<GraphComms>>> = const { OnceCell::new() };
}

/// Handle message sent from Main thread to worker.
async fn worker_msg(event: MessageEvent) -> Result<(), JsValue> {
    let scope = web_sys::js_sys::global().dyn_into::<DedicatedWorkerGlobalScope>()?;
    match event.data().try_into()? {
        MainToWorker::Start { samp_rate } => {
            // Run the decoder.
            let scope = web_sys::js_sys::global().dyn_into::<DedicatedWorkerGlobalScope>()?;
            let o = radio_1200(samp_rate).await?;
            scope
                .post_message(
                    &WorkerToMain::Result(o)
                        .try_into()
                        .expect("failed to serialize"),
                )
                .expect("failed to post message");
        }
        MainToWorker::Data(id, data) => {
            trace!("Worker: Got data on {id:?} len {}", data.len());
            GRAPH_COMMS.with(|cell| {
                let cell = cell.clone();
                let comms = cell.get().unwrap().clone();
                spawn_local(async move {
                    let comms = &mut RefCell::borrow_mut(&comms);
                    comms.src[&id]
                        .send(wasm_source::Msg::Extend(data))
                        .await
                        .expect("Worker failed to send data to the wasm source");
                    comms
                        .graph
                        .send(())
                        .await
                        .expect("Worker failed to send bump to graph");
                });
            });
        }
        MainToWorker::Eof(id) => {
            info!("Worker: Got EOF on {id:?}");
            // TODO: use the ID.
            GRAPH_COMMS.with(|cell| {
                let cell = cell.clone();
                let comms = cell.get().unwrap().clone();
                spawn_local(async move {
                    //let mut comms: &mut GraphComms = &mut RefCell::borrow_mut(&comms);
                    let comms = &mut RefCell::borrow_mut(&comms);
                    //let mut comms = comms.borrow_mut();
                    comms.src[&id].send(wasm_source::Msg::Eof).await.unwrap();
                    comms.graph.send(()).await.unwrap();
                });
            });
        }
        MainToWorker::Ping(t) => {
            info!("Worker: Got ping");
            scope
                .post_message(&WorkerToMain::Pong(t).try_into().unwrap())
                .expect("worker failed to send pong");
        }
        MainToWorker::Pong(from) => {
            let to = js_performance_now();
            info!("Worker: Got Pong {from} -> {to}: {}", to - from);
        }
    }
    Ok(())
}

/// Main entry point into the worker.
pub(crate) async fn setup() -> Result<(), JsValue> {
    info!("Setting up worker");

    let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        spawn_local(async move {
            if let Err(e) = worker_msg(event).await {
                // TODO: send error.
                info!("Worker message handler failed: {e:?}");
            }
        });
    });

    let global = web_sys::js_sys::global().dyn_into::<DedicatedWorkerGlobalScope>()?;
    global.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();
    global.post_message(&WorkerToMain::Ready.try_into()?)?;
    info!("Done setting up worker");

    Ok(())
}

/// Run 1200bps AX.25 decoder.
///
/// The input comes in via GraphComms into the WasmSource block, so this
/// function doesn't return until an EOF has come in.
async fn radio_1200(samp_rate: u64) -> rustradio::Result<String> {
    info!("AX.25 1200 decode of");

    // Decoder parameters.
    let samp_rate = samp_rate as f32;
    let if_rate = 50_000.0;
    let baud = 1200.0;
    let freq1 = 1200.0;
    let freq2 = 2200.0;
    let center_freq = freq1 + (freq2 - freq1) / 2.0;
    //let symbol_taps = vec![0.0001, 0.9999];
    let symbol_taps = vec![1.0];
    let max_deviation = 0.1;

    // Set up source block.
    let mut g = crate::wasm_graph::WasmGraph::new();
    let (src, prev, src_tx) = crate::wasm_source::WasmSource::new();
    g.add(Box::new(src));

    // Set up rest of decoder graph.
    let prev = blockchain![
        g,
        prev,
        Parse::new(prev),
        FftFilter::new(
            prev,
            rustradio::fir::low_pass_complex(
                samp_rate,
                20_000.0,
                100.0,
                &rustradio::window::WindowType::Hamming
            )
        ),
        RationalResampler::builder()
            .deci(samp_rate as usize)
            .interp(if_rate as usize)
            .build(prev)
            .map_err(|e| rustradio::Error::wrap(e, "rational resampler"))?,
    ];
    let prev = add_complex_mag_tap(&mut g, "iq_mag", prev);
    let prev = blockchain![
        g,
        prev,
        QuadratureDemod::new(prev, 1.0),
        Hilbert::new(prev, 65, &rustradio::window::WindowType::Hamming),
        QuadratureDemod::new(prev, 1.0),
        FftFilterFloat::new(
            prev,
            &rustradio::fir::low_pass(
                samp_rate,
                1100.0,
                100.0,
                &rustradio::window::WindowType::Hamming
            )
        ),
        add_const(prev, -center_freq * 2.0 * std::f32::consts::PI / if_rate),
        SymbolSync::new(
            prev,
            if_rate / baud,
            max_deviation,
            Box::new(rustradio::symbol_sync::TedZeroCrossing::new()),
            Box::new(rustradio::iir_filter::IirFilter::new(&symbol_taps))
        ),
        BinarySlicer::new(prev),
        NrziDecode::new(prev),
        HdlcDeframer::new(prev, 10, 1500),
    ];

    let (tx, rx) = async_channel::bounded(SOURCE_CHANNEL_SIZE);
    GRAPH_COMMS.with(|cell| {
        cell.get_or_init(move || {
            Rc::new(RefCell::new(GraphComms {
                src: [(crate::RECEIVER_SOURCE, src_tx)].into_iter().collect(),
                graph: tx,
            }))
        });
    });
    info!("Running graph");
    g.run_async(rx)
        .await
        .map_err(|e| rustradio::Error::wrap(e, "graph run"))?;
    let mut outs = Vec::new();
    while let Some(p) = prev.pop() {
        outs.push(format!("Decoded {p:?}").to_string());
    }
    let result = if outs.is_empty() {
        "nothing decoded".to_string()
    } else {
        outs.join("\n")
    };
    Ok(result)
}

/// Helper function to tee off into a FloatSink (to a graph).
fn add_complex_mag_tap(
    g: &mut crate::wasm_graph::WasmGraph,
    name: impl Into<String>,
    src: ReadStream<rustradio::Complex>,
) -> ReadStream<rustradio::Complex> {
    let (tee, src, tap_src) = Tee::new(src);
    let (mag, tap_src) = ComplexToMag2::new(tap_src);
    let sink = crate::float_sink::FloatSink::new(tap_src, name.into());
    g.add(Box::new(tee));
    g.add(Box::new(mag));
    g.add(Box::new(sink));
    src
}

// TODO: add support for 9600
#[allow(unused)]
async fn radio_wrap_9600() -> rustradio::Result<String> {
    info!("AX.25 9600 decode");
    let samp_rate = 50_000.0;
    let if_rate = 50_000.0;
    let baud = 9600.0;
    //let symbol_taps = vec![0.0001, 0.9999];
    let symbol_taps = vec![1.0];
    let max_deviation = 0.1;

    // Set up source block.
    let mut g = crate::wasm_graph::WasmGraph::new();
    let (src, prev, src_tx) = crate::wasm_source::WasmSource::new();
    g.add(Box::new(src));

    let prev = blockchain![
        g,
        prev,
        Parse::new(prev),
        FftFilter::new(
            prev,
            rustradio::fir::low_pass_complex(
                samp_rate,
                12_500.0,
                100.0,
                &rustradio::window::WindowType::Hamming
            )
        ),
        RationalResampler::builder()
            .deci(samp_rate as usize)
            .interp(if_rate as usize)
            .build(prev)
            .map_err(|e| rustradio::Error::wrap(e, "rational resampler"))?,
        QuadratureDemod::new(prev, 1.0),
        SymbolSync::new(
            prev,
            if_rate / baud,
            max_deviation,
            Box::new(rustradio::symbol_sync::TedZeroCrossing::new()),
            Box::new(rustradio::iir_filter::IirFilter::new(&symbol_taps))
        ),
        BinarySlicer::new(prev),
        NrziDecode::new(prev),
        Descrambler::g3ruh(prev),
        HdlcDeframer::new(prev, 10, 1500),
    ];

    info!("Running graph");
    g.run()
        .map_err(|e| rustradio::Error::wrap(e, "graph run"))?;
    Ok(match prev.pop() {
        None => "nothing decoded".to_string(),
        Some(p) => format!("Decoded {p:?}").to_string(),
    })
}
