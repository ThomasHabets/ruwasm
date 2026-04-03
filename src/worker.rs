use std::cell::OnceCell;
use std::cell::RefCell;
use std::rc::Rc;

use rustradio::blockchain;
use rustradio::blocks::*;
use rustradio::graph::{Graph, GraphRunner};

use futures::SinkExt;
use serde_wasm_bindgen::{from_value, to_value};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};
// use futures::channel::mpsc;
use futures_channel::mpsc;
//use wasmer_types::lib::std::sync::mpsc;
//use tokio::sync::mpsc;

use crate::log;
use crate::wasm_source;
use crate::{MainToWorker, WorkerToMain};

struct GraphComms {
    src: std::sync::mpsc::Sender<crate::wasm_source::Msg>,
    graph: async_channel::Sender<()>,
}

thread_local! {
    static GRAPH_COMMS: OnceCell<Rc<RefCell<GraphComms>>> = const { OnceCell::new() };
}

async fn worker_msg(scope: DedicatedWorkerGlobalScope, event: MessageEvent) -> Result<(), JsValue> {
    match from_value::<MainToWorker>(event.data()).expect("parsing MainToWorker message") {
        MainToWorker::Data(data) => {
            log(&format!("Worker: Got data len {}", data.len()));
            GRAPH_COMMS.with(|cell| {
                let cell = cell.clone();
                let comms = cell.get().unwrap().clone();
                spawn_local(async move {
                    //let mut comms: &mut GraphComms = &mut RefCell::borrow_mut(&comms);
                    let comms = &mut RefCell::borrow_mut(&comms);
                    //let mut comms = comms.borrow_mut();
                    comms.src.send(wasm_source::Msg::Extend(data)).unwrap();
                    comms.graph.send(()).await.unwrap();
                });
            });
            /*
            let o = radio_1200(&data).await.expect("rustradio run failed");
            log(&format!("Worker run returned: {o}"));
            scope
                .post_message(&to_value(&WorkerToMain::Result(o)).expect("failed to serialize"))
                .expect("failed to post message");
                */
        }
        MainToWorker::Eof => {
            log("Worker: Got EOF");
            GRAPH_COMMS.with(|cell| {
                let cell = cell.clone();
                let comms = cell.get().unwrap().clone();
                spawn_local(async move {
                    //let mut comms: &mut GraphComms = &mut RefCell::borrow_mut(&comms);
                    let comms = &mut RefCell::borrow_mut(&comms);
                    //let mut comms = comms.borrow_mut();
                    comms.src.send(wasm_source::Msg::Eof).unwrap();
                    comms.graph.send(()).await.unwrap();
                });
            });
        }
        MainToWorker::Ping => {
            log("Worker: Got ping");
            /*
            GRAPH_COMMS.with(|cell| {
                let cell = cell.clone();
                let comms = cell.get().unwrap().clone();
                spawn_local(async move {
                    //let mut comms: &mut GraphComms = &mut RefCell::borrow_mut(&comms);
                    let comms = &mut RefCell::borrow_mut(&comms);
                    //let mut comms = comms.borrow_mut();
                    comms.src.send(wasm_source::Msg::Eof).unwrap();
                    comms.graph.send(()).await.unwrap();
                });
            });
            */
        }
        MainToWorker::Pong => {}
    }
    Ok(())
}

pub(crate) async fn setup() -> Result<(), JsValue> {
    log("Setting up worker");

    let global = web_sys::js_sys::global().dyn_into::<DedicatedWorkerGlobalScope>()?;

    let worker = global.clone();
    let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let worker = worker.clone();
        spawn_local(async move {
            if let Err(e) = worker_msg(worker, event).await {
                // TODO: send error.
                log(&format!("Worker message handler failed: {e:?}"));
            }
        });
    });

    global.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();
    global.post_message(&to_value(&WorkerToMain::Ready)?)?;
    log("Done setting up worker");

    // Run the decoder.
    let scope = web_sys::js_sys::global().dyn_into::<DedicatedWorkerGlobalScope>()?;
    let o = radio_1200(&[]).await.expect("rustradio run failed");
    scope
        .post_message(&to_value(&WorkerToMain::Result(o)).expect("failed to serialize"))
        .expect("failed to post message");
    Ok(())
}

async fn radio_1200(data: &[u8]) -> rustradio::Result<String> {
    log(&format!("AX.25 1200 decode of {} bytes", data.len()));

    // Decoder parameters.
    let samp_rate = 50_000.0;
    let if_rate = 50_000.0;
    let baud = 1200.0;
    let freq1 = 1200.0;
    let freq2 = 2200.0;
    let center_freq = freq1 + (freq2 - freq1) / 2.0;
    //let symbol_taps = vec![0.0001, 0.9999];
    let symbol_taps = vec![1.0];
    let max_deviation = 0.1;

    // Set up source part.
    let mut g = crate::wasm_graph::WasmGraph::new();
    //let src = VectorSource::new(data.to_vec());
    let (src, prev, src_tx) = crate::wasm_source::WasmSource::new();
    /*
    src_tx
        .send(crate::wasm_source::Msg::Eof)
        .map_err(|_| rustradio::Error::msg("src_tx send eof"))?;
        */
    src_tx
        .send(crate::wasm_source::Msg::Extend(data.to_vec()))
        .map_err(|_| rustradio::Error::msg("src_tx send extend"))?;
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

    // TODO: magic value.
    let (tx, rx) = async_channel::unbounded();
    GRAPH_COMMS.with(|cell| {
        cell.get_or_init(move || {
            Rc::new(RefCell::new(GraphComms {
                src: src_tx,
                graph: tx,
            }))
        });
    });
    log(&format!("Running graph"));
    g.run_async(rx)
        .await
        .map_err(|e| rustradio::Error::wrap(e, "graph run"))?;
    let mut outs = Vec::new();
    while let Some(p) = prev.pop() {
        outs.push(format!("Decoded {p:?}").to_string());
    }
    Ok(if outs.is_empty() {
        "nothing decoded".to_string()
    } else {
        outs.join("\n")
    })
}

// TODO: add support for 9600
#[allow(unused)]
fn radio_wrap_9600(data: &[u8]) -> rustradio::Result<String> {
    log(&format!("AX.25 9600 decode of {} bytes", data.len()));
    let samp_rate = 50_000.0;
    let if_rate = 50_000.0;
    let baud = 9600.0;
    //let symbol_taps = vec![0.0001, 0.9999];
    let symbol_taps = vec![1.0];
    let max_deviation = 0.1;
    let mut g = Graph::new();
    let prev = blockchain![
        g,
        prev,
        VectorSource::new(data.to_vec()),
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

    log(&format!("Running graph"));
    g.run()
        .map_err(|e| rustradio::Error::wrap(e, "graph run"))?;
    Ok(match prev.pop() {
        None => "nothing decoded".to_string(),
        Some(p) => format!("Decoded {p:?}").to_string(),
    })
}
