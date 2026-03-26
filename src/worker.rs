use rustradio::blockchain;
use rustradio::blocks::*;
use rustradio::graph::{Graph, GraphRunner};
use serde_wasm_bindgen::{from_value, to_value};
use wasm_bindgen::prelude::*;
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

use crate::log;
use crate::{MainToWorker, WorkerToMain};

pub(crate) async fn setup() -> Result<(), JsValue> {
    log("Setting up worker");
    let global = web_sys::js_sys::global().dyn_into::<DedicatedWorkerGlobalScope>()?;

    let worker = global.clone();
    let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        match from_value::<MainToWorker>(event.data()).expect("parsing MainToWorker message") {
            MainToWorker::Data(data) => {
                let o = radio_1200(&data).expect("rustradio run failed");
                log(&format!("Worker run returned: {o}"));
                worker
                    .post_message(&to_value(&WorkerToMain::Result(o)).expect("failed to serialize"))
                    .expect("failed to post message");
            }
            MainToWorker::Ping => {}
            MainToWorker::Pong => {}
        }
        /*
        if let Some(text) = event.data().as_string() {
            web_sys::console::log_1(&format!("worker received: {text}").into());

            let reply = format!("pong: {text}");
            worker.post_message(&JsValue::from_str(&reply)).unwrap();
        } else {
            worker
                .post_message(&JsValue::from_str("worker got non-string message"))
                .unwrap();
        }
        */
    });

    global.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();
    global.post_message(&to_value(&WorkerToMain::Ready)?)?;
    log("Done setting up worker");
    Ok(())
}

fn radio_1200(data: &[u8]) -> rustradio::Result<String> {
    log(&format!("AX.25 1200 decode of {} bytes", data.len()));
    let samp_rate = 50_000.0;
    let if_rate = 50_000.0;
    let baud = 1200.0;
    let freq1 = 1200.0;
    let freq2 = 2200.0;
    let center_freq = freq1 + (freq2 - freq1) / 2.0;
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

    log(&format!("Running graph"));
    g.run()
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
