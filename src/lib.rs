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
pub fn radio() -> String {
    radio_wrap().expect("oh no")
}

fn radio_wrap() -> rustradio::Result<String> {
    log(&format!("AX.25 9600 decode"));
    let samp_rate = 50_000.0;
    let if_rate = 50_000.0;
    let baud = 9600.0;
    let symbol_taps = vec![0.0001, 0.9999];
    let max_deviation = 0.1;
    let mut g = Graph::new();
    let prev = blockchain![
        g,
        prev,
        VectorSource::new(vec![Complex::default()]),
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
            .build(prev)?,
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
    g.run()?;
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
