use serde::Serialize;
use wasm_bindgen::prelude::*;

use rustradio::block::Block;
use rustradio::blocks::*;
use rustradio::graph::{Graph, GraphRunner};
use rustradio::{Complex, blockchain};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

#[wasm_bindgen]
#[derive(Serialize)]
pub struct Return {
    a: i32,
    b: i32,
    sum: i32,
    eval: String,
}

#[wasm_bindgen]
pub fn git_version() -> String {
    env!("GIT_VERSION").to_string()
}

#[wasm_bindgen]
pub fn rustc_version() -> String {
    env!("RUSTC_VERSION").to_string()
}

#[wasm_bindgen]
pub fn compute(n: u32) -> u32 {
    (0..n).map(|x| x * x).sum()
}

#[wasm_bindgen]
pub fn add(a: i32, b: i32) -> String {
    log(&format!("Hello world, adding {a} and {b}"));
    serde_json::to_string(&Return {
        a,
        b,
        sum: a + b,
        eval: "console.log('hello world')".to_string(),
    })
    .unwrap()
}

#[wasm_bindgen]
pub fn radio(data: &[u8]) -> String {
    match radio_wrap_1200(data) {
        Ok(s) => s,
        Err(e) => format!("Error: {e}").to_string(),
    }
}

fn radio_wrap_1200(data: &[u8]) -> rustradio::Result<String> {
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
        //Descrambler::g3ruh(prev),
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

#[wasm_bindgen]
pub fn radio2(a: i32, b: i32) -> String {
    log(&format!("Hello radio, adding {a} and {b}"));
    let (mut b1, src1) = VectorSource::new(vec![a]);
    b1.work().unwrap();
    let (mut b2, src2) = VectorSource::new(vec![b]);
    b2.work().unwrap();
    let (mut b3, out) = Add::new(src1, src2);
    b3.work().unwrap();
    let (r, _) = out.read_buf().unwrap();
    let o = r.slice();
    format!("Result is now {o:?}").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
}
