use serde::Serialize;
use wasm_bindgen::prelude::*;

use rustradio::block::Block;
use rustradio::blocks::*;

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
pub fn radio(a: i32, b: i32) -> String {
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
