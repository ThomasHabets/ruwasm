use std::cell::OnceCell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use rustradio::blockchain;
#[allow(clippy::wildcard_imports)]
use rustradio::blocks::*;
use rustradio::data_stream::DataStreamId;
use rustradio::graph::GraphRunner;
use rustradio::stream::ReadStream;

use log::{debug, error, info, trace, warn};
use serde::Serialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

use crate::Ax25Messages;
use crate::data_stream::{DataStream, Event as DataStreamEvent, RequestState};
use crate::js_performance_now;
use crate::receiver_source;
use crate::wasm_source;
use crate::{MainToWorker, WorkerToMain, WorkerToMainRef};

// TODO: magic values.
const SOURCE_CHANNEL_SIZE: usize = 10;
const IF_SAMPLE_RATE: usize = 50_000;
const VIZ_SAMPLE_RATE: usize = 1_000;
const SPECTRUM_SIZE: usize = 1024;

/// Channels used to pass source data into a running graph.
struct GraphComms {
    src: HashMap<DataStreamId, async_channel::Sender<crate::wasm_source::Msg>>,
    graph: async_channel::Sender<()>,
}

thread_local! {
    static GRAPH_COMMS: OnceCell<Rc<futures_intrusive::sync::LocalMutex<GraphComms>>> = const { OnceCell::new() };
    static DATA_STREAM: RefCell<DataStream> = RefCell::new(DataStream::new());
}

#[allow(clippy::enum_glob_use)]
fn forget_shared<T: rustradio_ui::ApplicationSpecific>(msg: rustradio_ui::WorkerToMain<T>) {
    use rustradio_ui::WorkerToMain::*;
    match msg {
        // Ignore all the messages that don't have shared data.
        Ready(_)
        | ApplicationSpecific(_)
        | Ping(_)
        | Pong(_)
        | DataStream(_)
        | End(_)
        | LogLine { .. }
        | FloatStreams(_)
        | ComplexStreams(_) => {}
        SharedFloat(_, v) => {
            for e in v {
                e.forget();
            }
        }
        SharedComplex(_, v) => {
            for e in v {
                e.forget();
            }
        }
    }
}

#[allow(clippy::enum_glob_use)]
fn drop_shared<T: rustradio_ui::ApplicationSpecific>(msg: rustradio_ui::WorkerToMain<T>) {
    use rustradio_ui::WorkerToMain::*;
    match msg {
        // Ignore all the messages that don't have shared data.
        Ready(_)
        | ApplicationSpecific(_)
        | Ping(_)
        | Pong(_)
        | DataStream(_)
        | End(_)
        | LogLine { .. }
        | FloatStreams(_)
        | ComplexStreams(_) => {}
        SharedFloat(_, v) => {
            for e in v {
                let _ = e.into_vec();
            }
        }
        SharedComplex(_, v) => {
            for e in v {
                let _ = e.into_vec();
            }
        }
    }
}

/// Post a message to the main UI.
pub(crate) fn post_message(msg: WorkerToMain) -> Result<(), JsValue> {
    match post_message_ref(&msg) {
        Ok(()) => {
            forget_shared(msg);
            Ok(())
        }
        Err(e) => {
            drop_shared(msg);
            Err(e)
        }
    }
}

pub(crate) fn post_message_ref<T: Serialize + ?Sized>(msg: &T) -> Result<(), JsValue> {
    let msg = serde_wasm_bindgen::to_value(msg)?;
    let scope = web_sys::js_sys::global().dyn_into::<DedicatedWorkerGlobalScope>()?;
    scope.post_message(&msg)
}

/// Send one control or data message to the graph source for a receiver.
async fn send_source_msg(id: DataStreamId, msg: wasm_source::Msg) -> Result<(), JsValue> {
    let Some(comms) = GRAPH_COMMS.with(|cell| {
        let cell = cell.clone();
        cell.get().map(Clone::clone)
    }) else {
        warn!("Worker: graph comms not ready for {id}");
        return Ok(());
    };

    let Some((src, graph)) = ({
        let comms = comms.lock().await;
        comms
            .src
            .get(&id)
            .map(|src| (src.clone(), comms.graph.clone()))
    }) else {
        warn!("Worker: no receiver registered for {id}");
        return Ok(());
    };

    src.send(msg)
        .await
        .map_err(|e| JsValue::from_str(&format!("source receiver send failed: {e:?}")))?;
    graph
        .send(())
        .await
        .map_err(|e| JsValue::from_str(&format!("graph bump send failed: {e:?}")))?;
    Ok(())
}

/// Request more bytes for a WasmSource through the DATA_STREAM protocol.
pub(crate) fn request_receiver_data(
    receiver: &DataStreamId,
    _pos: u64,
    size: u64,
) -> rustradio::Result<()> {
    let request_state = DATA_STREAM.with(|data_stream| {
        data_stream
            .borrow_mut()
            .request_data(receiver, size as usize)
    })?;

    match request_state {
        RequestState::Waiting => Ok(()),
        RequestState::Issued(packet) => {
            post_message(WorkerToMain::DataStream(packet))
                .map_err(|e| rustradio::Error::msg(format!("{e:?}")))?;
            Ok(())
        }
    }
}

/// Apply inbound DATA_STREAM bytes from main or WebSocket to graph sources.
async fn handle_data_stream_bytes(data: &[u8]) -> Result<(), JsValue> {
    if data.is_empty() {
        let receivers = DATA_STREAM.with(|data_stream| data_stream.borrow_mut().disconnect());
        let receivers = if receivers.is_empty() {
            vec![receiver_source()]
        } else {
            receivers
        };
        for receiver in receivers {
            send_source_msg(receiver, wasm_source::Msg::Eof).await?;
        }
        return Ok(());
    }

    // TODO: can we create borrowed events instead of copying them?
    let events = DATA_STREAM
        .with(|data_stream| data_stream.borrow_mut().handle_bytes(data))
        .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;

    for event in events {
        match event {
            DataStreamEvent::PeerReady => {
                let receivers = DATA_STREAM.with(|data_stream| data_stream.borrow().receivers());
                for receiver in receivers {
                    send_source_msg(receiver, wasm_source::Msg::RetryRequest).await?;
                }
            }
            DataStreamEvent::Data { receiver, data } => {
                send_source_msg(receiver, wasm_source::Msg::Extend(data)).await?;
            }
            DataStreamEvent::Eof { receiver } => {
                send_source_msg(receiver, wasm_source::Msg::Eof).await?;
            }
        }
    }

    Ok(())
}

/// Handle message sent from Main thread to worker.
async fn worker_msg(event: MessageEvent) -> Result<(), JsValue> {
    match event.data().try_into()? {
        MainToWorker::SharedByte(name, streams) => {
            assert_eq!(name, "rtl-sdr");
            let streams: Vec<_> = streams
                .into_iter()
                .map(rustradio_ui::SharedVecPtr::into_vec)
                .collect();
            //handle_data_stream_bytes(&bytes).await?;
            // TODO: avoid this copy.
            send_source_msg(
                name.clone().into(),
                wasm_source::Msg::Extend(streams[0].data.clone()),
            )
            .await?;
        }
        MainToWorker::Start(crate::Ax25Start { samp_rate, rtlsdr }) => {
            debug!("Got MainToWorker::Start");
            // Run the decoder.
            let o = radio_1200(samp_rate, rtlsdr).await?;
            // Using reference serialization here doesn't actually help, but it
            // does work.
            post_message_ref(&WorkerToMainRef::End(crate::Ax25EndRef { s: &o }))?;
        }
        MainToWorker::DataStream(data) => {
            trace!("Worker: Got DATA_STREAM bytes len {}", data.len());
            handle_data_stream_bytes(&data).await?;
        }
        MainToWorker::Ping(t) => {
            info!("Worker: Got ping");
            post_message(WorkerToMain::Pong(t))?;
        }
        MainToWorker::Pong(from) => {
            let to = js_performance_now();
            info!("Worker: Got Pong {from} -> {to}: {}", to - from);
        }
    }
    Ok(())
}

/// Main entry point into the worker.
#[allow(clippy::unused_async)]
pub(crate) async fn setup() -> Result<(), JsValue> {
    info!("Setting up worker");

    let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        // TODO: is `spawn_local` guaranteed to enqueue these messages in order?
        // If not, then data may arrive out of order.
        //
        // If that's the case, then we're going to have to queue up the data
        // syncly somehow. Maybe by adding a hop through an mpsc. That or remove
        // a level of indirection. :-)
        spawn_local(async move {
            if let Err(e) = worker_msg(event).await {
                info!("Worker message handler failed: {e:?}");
            }
        });
    });

    let global = web_sys::js_sys::global().dyn_into::<DedicatedWorkerGlobalScope>()?;
    global.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();

    // Set messageerror handler.
    let onmsgerr =
        Closure::<dyn FnMut(MessageEvent) -> Result<(), JsValue>>::new(move |e: MessageEvent| {
            error!("Worker: Message Error: {e:?}");
            Ok(())
        });
    global.set_onmessageerror(Some(onmsgerr.as_ref().unchecked_ref()));
    onmsgerr.forget();

    post_message(WorkerToMain::Ready(crate::Ax25Ready {}))?;
    info!("Done setting up worker");

    Ok(())
}

/// Run 1200bps AX.25 decoder.
///
/// The input comes in via GraphComms into the WasmSource block, so this
/// function doesn't return until an EOF has come in.
#[allow(clippy::too_many_lines)]
async fn radio_1200(samp_rate: u64, rtlsdr: bool) -> rustradio::Result<String> {
    info!("AX.25 1200 decoder running with sample rate {samp_rate} IF rate {IF_SAMPLE_RATE}");

    // Decoder parameters.
    let samp_rate = samp_rate as f32;
    let if_rate = IF_SAMPLE_RATE as f32;
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

    let prev = if rtlsdr {
        blockchain![g, prev, RtlSdrDecode::new(prev)]
    } else {
        blockchain![g, prev, Parse::new(prev)]
    };

    // Set up rest of decoder graph.
    let prev = blockchain![
        g,
        prev,
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
    let prev = add_spectrum_tap(&mut g, prev);
    let prev = add_viz_taps(&mut g, prev)?;
    let prev = blockchain![
        g,
        prev,
        QuadratureDemod::new(prev, 1.0),
        Hilbert::new(prev, 65, &rustradio::window::WindowType::Hamming),
        QuadratureDemod::new(prev, 1.0),
        FftFilterFloat::new(
            prev,
            &rustradio::fir::low_pass(
                if_rate,
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
        NCMap::new(prev, "log_packet", |x, tags| {
            info!("Found packet! {x:?}");
            match rax25::Packet::parse(&x, None) {
                Ok(packet) => {
                    if let rax25::PacketType::Ui(ui) = packet.packet_type() {
                        let payload = String::from_utf8_lossy(&ui.payload);
                        info!("Parsed: {packet:?} / Payload as string: {payload:?}");
                    } else if let rax25::PacketType::Iframe(i) = packet.packet_type() {
                        let payload = String::from_utf8_lossy(&i.payload);
                        info!("Parsed: {packet:?} / Payload as string: {payload:?}");
                    } else {
                        info!("Parsed: {packet:?}");
                    }
                    post_message(WorkerToMain::ApplicationSpecific(Ax25Messages::Decoded(
                        format!("Last decode: {packet:?}"),
                    )))
                    .unwrap();
                }
                Err(e) => {
                    info!("Packet did not decode: {e}");
                }
            }
            vec![(x, tags)]
        }),
    ];

    let (tx, rx) = async_channel::bounded(SOURCE_CHANNEL_SIZE);
    DATA_STREAM.with(|data_stream| {
        data_stream
            .borrow_mut()
            .register_receiver(receiver_source());
    });
    GRAPH_COMMS.with(|cell| {
        cell.get_or_init(move || {
            // Need `is_fair` to be set, because data packets need to come in
            // order.
            Rc::new(futures_intrusive::sync::LocalMutex::new(
                GraphComms {
                    src: [(receiver_source(), src_tx)].into_iter().collect(),
                    graph: tx,
                },
                true,
            ))
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

/// Tee off the downsampled IF stream into FFT spectrum frames for the UI.
fn add_spectrum_tap(
    g: &mut crate::wasm_graph::WasmGraph,
    src: ReadStream<rustradio::Complex>,
) -> ReadStream<rustradio::Complex> {
    let (tee, src, prev) = Tee::new(src);
    g.add(Box::new(tee));

    let prev = blockchain![
        g,
        prev,
        FftStream::new(prev, SPECTRUM_SIZE),
        Map::keep_tags(prev, "fft_power_db", |bin: rustradio::Complex| {
            let power = (bin.norm_sqr() / SPECTRUM_SIZE as f32).max(1.0e-20);
            10.0 * power.log10()
        }),
        StreamToPdu::new(prev, rustradio::fft_stream::TAG_FRAME, SPECTRUM_SIZE, 1),
        PduAverage::new(prev, 10),
    ];
    let sink =
        crate::float_pdu_sink::FloatPduSink::new(prev, "iq_spectrum".into(), IF_SAMPLE_RATE as f32);
    g.add(Box::new(sink));

    src
}

/// Tee off one downsampled complex stream for the visualization sinks.
fn add_viz_taps(
    g: &mut crate::wasm_graph::WasmGraph,
    src: ReadStream<rustradio::Complex>,
) -> rustradio::Result<ReadStream<rustradio::Complex>> {
    // TODO: if you change this, change the time
    // sink value in mainthread.rs too.
    // and also in time_sink for max number of samples.
    let (input_tee, src, prev) = Tee::new(src);
    g.add(Box::new(input_tee));

    let prev = blockchain![
        g,
        prev,
        RationalResampler::builder()
            .deci(IF_SAMPLE_RATE)
            .interp(VIZ_SAMPLE_RATE)
            .build(prev)?
    ];

    let (viz_tee, constellation_prev, mag_prev) = Tee::new(prev);
    g.add(Box::new(viz_tee));

    let mag_prev = blockchain![g, mag_prev, ComplexToMag2::new(mag_prev)];

    let constellation_sink =
        crate::complex_sink::ComplexSink::new(constellation_prev, "iq_constellation".into());
    let mag_sink = crate::float_sink::FloatSink::new(mag_prev, "iq_mag".into());
    g.add(Box::new(constellation_sink));
    g.add(Box::new(mag_sink));

    Ok(src)
}

// TODO: add support for 9600
#[allow(unused)]
#[allow(clippy::unused_async)]
async fn radio_wrap_9600(iq: bool) -> rustradio::Result<String> {
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

    let prev = if iq {
        blockchain![g, prev, Parse::new(prev)]
    } else {
        blockchain![g, prev, RtlSdrDecode::new(prev)]
    };

    let prev = blockchain![
        g,
        prev,
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
        NCMap::new(prev, "log_packet", |x, tags| {
            info!("Decoded packet! {x:?}");
            vec![(x, tags)]
        }),
    ];

    info!("Running graph");
    g.run()
        .map_err(|e| rustradio::Error::wrap(e, "graph run"))?;
    Ok(match prev.pop() {
        None => "nothing decoded".to_string(),
        Some(p) => format!("Decoded {p:?}").to_string(),
    })
}
