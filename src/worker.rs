use std::cell::OnceCell;
use std::collections::HashMap;
use std::rc::Rc;

use rustradio::Float;
use rustradio::blockchain;
#[allow(clippy::wildcard_imports)]
use rustradio::blocks::*;
use rustradio::graph::GraphRunner;
use rustradio::stream::ReadStream;
use rustradio_ui::worker::{post_message, send_message, send_message_sync, source};
use rustradio_ui::{BootstrapMpsc, TaggedVec};

use log::{error, info, warn};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

use crate::Ax25Messages;
use crate::RECEIVER_SOURCE_ID;
use crate::js_performance_now;
use crate::{MainToWorker, WorkerToMain};

type FloatSink = rustradio_ui::worker::FloatSink<crate::Ax25WorkerToMain>;
type ComplexSink = rustradio_ui::worker::ComplexSink<crate::Ax25WorkerToMain>;
type FloatPduSink = rustradio_ui::worker::FloatPduSink<crate::Ax25WorkerToMain>;

// TODO: magic values.
const SOURCE_CHANNEL_SIZE: usize = 10;
const AUDIO_SAMPLE_RATE: usize = 44_100;
pub(crate) const IF_SAMPLE_RATE: usize = 50_000;
pub(crate) const VIZ_SAMPLE_RATE: usize = 1_000;
const SPECTRUM_SIZE: usize = 256;

/// Channels used to pass source data into a running graph.
struct GraphComms {
    src: HashMap<String, async_channel::Sender<source::Msg<u8>>>,
    graph: async_channel::Sender<()>,
}

thread_local! {
    static GRAPH_COMMS: OnceCell<Rc<futures_intrusive::sync::LocalMutex<GraphComms>>> = const { OnceCell::new() };
}

/// Send one control or data message to the graph source for a receiver.
async fn send_source_msg(id: &str, msg: source::Msg<u8>) -> Result<(), JsValue> {
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
            .get(id)
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

/// Convert one shared-memory byte message into a graph-source update.
fn source_msg_from_bytes(streams: Vec<TaggedVec<u8>>) -> source::Msg<u8> {
    let mut streams = streams.into_iter();
    let mut data = streams.next().map(|stream| stream.data).unwrap_or_default();
    for stream in streams {
        data.extend(stream.data);
    }
    if data.is_empty() {
        source::Msg::Eof
    } else {
        source::Msg::Extend(data)
    }
}

/// Handle message sent from Main thread to worker.
async fn worker_msg(msg: MainToWorker) -> Result<(), JsValue> {
    match msg {
        MainToWorker::BootstrapMpsc(b) => {
            info!("Received main channel endpoints");
            let BootstrapMpsc { tx, rx } =
                BootstrapMpsc::<crate::Ax25MainToWorker, crate::Ax25WorkerToMain>::from_ptr(b);
            rustradio_ui::worker::set_main_ui_tx(tx);
            spawn_local(async move {
                while let Ok(msg) = rx.recv().await {
                    if let Err(e) = worker_msg(msg).await {
                        error!("Failed to process message {e:?}");
                    }
                }
            });
        }
        MainToWorker::Start(crate::Ax25Start {
            samp_rate,
            offset,
            rtlsdr,
        }) => {
            info!("Got MainToWorker::Start sample rate {samp_rate} offset {offset}");
            // Run the decoder.
            let s = radio_1200(samp_rate, offset, rtlsdr).await?;
            // Using reference serialization here doesn't actually help, but it
            // does work.
            send_message(WorkerToMain::End(crate::Ax25End { s })).await?;
        }
        MainToWorker::Bytes(name, streams) => {
            send_source_msg(&name, source_msg_from_bytes(streams)).await?;
        }
        MainToWorker::Ping(t) => {
            info!("Worker: Got ping");
            post_message(&WorkerToMain::Pong(t))?;
        }
        MainToWorker::Pong(from) => {
            let to = js_performance_now();
            info!("Worker: Got Pong {from} -> {to}: {}", to - from);
        }
        other => {
            info!("Got unknown {other:?}");
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
            match event.data().try_into() {
                Ok(msg) => {
                    match &msg {
                        MainToWorker::BootstrapMpsc(_) => {}
                        _other => warn!("Worker received posted {msg:?}"),
                    }
                    if let Err(e) = worker_msg(msg).await {
                        info!("Worker message handler failed: {e:?}");
                    }
                }
                Err(e) => error!("Failed to deserialize event {event:?}: {e:?}"),
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

    post_message(&WorkerToMain::Ready(crate::Ax25Ready {}))?;
    info!("Done setting up worker");

    Ok(())
}

/// Run 1200bps AX.25 decoder.
///
/// The input comes in via GraphComms into the WasmSource block, so this
/// function doesn't return until an EOF has come in.
#[allow(clippy::too_many_lines)]
async fn radio_1200(samp_rate: u64, offset: Float, rtlsdr: bool) -> rustradio::Result<String> {
    info!(
        "AX.25 1200 decoder running with sample rate {samp_rate} IF rate {IF_SAMPLE_RATE}, offset {offset}"
    );

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
    let (src, prev, src_tx) =
        source::WasmSource::<crate::Ax25WorkerToMain, _>::new(crate::RECEIVER_SOURCE_ID);
    g.add(Box::new(src));

    let prev = if rtlsdr {
        blockchain![g, prev, RtlSdrDecode::new(prev)]
    } else {
        blockchain![g, prev, Parse::new(prev)]
    };

    let filter1 = rustradio::fir::low_pass_complex(
        samp_rate,
        10_000.0,
        15_000.0,
        &rustradio::window::WindowType::Hamming,
    );
    info!("Taps on first filter: {}", filter1.len());

    // Filter and downsample raw signal.
    let prev = {
        let if_sample_rate = IF_SAMPLE_RATE as Float;
        let deci = (samp_rate / if_sample_rate) as usize;
        let if1_samp_rate = (samp_rate as usize) / deci;

        let mut prev = blockchain![
            g,
            prev,
            FirFilter::builder(filter1)
                .deci(deci)
                .translate(samp_rate, offset)
                .build(prev),
        ];

        // If off by more than 2%, add a rational resampler.
        let percent = ((if1_samp_rate as f32) - if_sample_rate).abs() / if_sample_rate;
        info!("FIR decimation output off by {}%", percent * 100.0);
        if percent > 0.02 {
            info!("Adding a rational resampler from {if1_samp_rate} to {IF_SAMPLE_RATE}");
            prev = blockchain![
                g,
                prev,
                RationalResampler::builder()
                    .deci(if1_samp_rate)
                    .interp(IF_SAMPLE_RATE)
                    .build(prev)?,
            ];
        }
        prev
    };

    // Set up rest of decoder graph.
    let prev = add_spectrum_tap(&mut g, prev);
    let prev = add_viz_taps(&mut g, prev)?;
    let prev = blockchain![g, prev, QuadratureDemod::new(prev, 1.0)];
    let prev = add_audio_tap(&mut g, prev)?;
    let audio_filter = rustradio::fir::low_pass(
        if_rate,
        1100.0,
        3900.0,
        &rustradio::window::WindowType::Hamming,
    );
    info!("Audio filter taps: {}", audio_filter.len());
    let prev = blockchain![
        g,
        prev,
        Hilbert::new(prev, 65, &rustradio::window::WindowType::Hamming),
        QuadratureDemod::new(prev, 1.0),
        FirFilter::new(prev, &audio_filter),
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
                    if let Err(e) = send_message_sync(WorkerToMain::ApplicationSpecific(
                        Ax25Messages::Decoded(format!("Last decode: {packet:?}")),
                    )) {
                        error!("Failed to send packet for logging: {e:?}");
                    }
                }
                Err(e) => {
                    info!("Packet did not decode: {e}");
                }
            }
            vec![(x, tags)]
        }),
    ];

    let (tx, rx) = async_channel::bounded(SOURCE_CHANNEL_SIZE);
    GRAPH_COMMS.with(|cell| {
        cell.get_or_init(move || {
            // Need `is_fair` to be set, because data packets need to come in
            // order.
            Rc::new(futures_intrusive::sync::LocalMutex::new(
                GraphComms {
                    src: [(RECEIVER_SOURCE_ID.to_string(), src_tx)]
                        .into_iter()
                        .collect(),
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
    let sink = FloatPduSink::new(prev, "iq_spectrum".into());
    g.add(Box::new(sink));

    src
}

/// Tee off demodulated audio after the first FM demodulator.
fn add_audio_tap(
    g: &mut crate::wasm_graph::WasmGraph,
    src: ReadStream<rustradio::Float>,
) -> rustradio::Result<ReadStream<rustradio::Float>> {
    let (tee, src, audio) = Tee::new(src);
    g.add(Box::new(tee));

    let audio = blockchain![
        g,
        audio,
        RationalResampler::builder()
            .deci(IF_SAMPLE_RATE)
            .interp(AUDIO_SAMPLE_RATE)
            .build(audio)?
    ];
    let sink = FloatSink::new(audio, "audio_demod".into());
    g.add(Box::new(sink));

    Ok(src)
}

/// Tee off one downsampled complex stream for the visualization sinks.
fn add_viz_taps(
    g: &mut crate::wasm_graph::WasmGraph,
    src: ReadStream<rustradio::Complex>,
) -> rustradio::Result<ReadStream<rustradio::Complex>> {
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

    let constellation_sink = ComplexSink::new(constellation_prev, "iq_constellation".into());
    let mag_sink = FloatSink::new(mag_prev, "iq_mag".into());
    g.add(Box::new(constellation_sink));
    g.add(Box::new(mag_sink));

    Ok(src)
}

#[cfg(test)]
mod tests {
    use super::source_msg_from_bytes;
    use rustradio_ui::TaggedVec;
    use rustradio_ui::worker::source::Msg;

    #[test]
    fn empty_byte_message_is_eof() {
        let streams = vec![TaggedVec {
            data: Vec::new(),
            tags: Vec::new(),
        }];
        assert!(matches!(source_msg_from_bytes(streams), Msg::Eof));
    }

    #[test]
    fn byte_streams_are_joined_in_order() {
        let streams = vec![
            TaggedVec {
                data: vec![1, 2],
                tags: Vec::new(),
            },
            TaggedVec {
                data: vec![3, 4],
                tags: Vec::new(),
            },
        ];
        match source_msg_from_bytes(streams) {
            Msg::Extend(data) => assert_eq!(data, [1, 2, 3, 4]),
            Msg::Eof => panic!("expected source bytes"),
        }
    }
}
